use byte_slice_cast::*;
use gst::prelude::*;
use gst_app;
use gst_audio;
use servo_media_audio::decoder::{AudioDecoder, AudioDecoderCallbacks};
use servo_media_audio::decoder::{AudioDecoderError, AudioDecoderOptions};
use std::sync::{
    mpsc::{self, Receiver},
    Arc, Mutex,
};

pub struct GStreamerAudioDecoderProgress(gst::buffer::MappedBuffer<gst::buffer::Readable>);

impl AsRef<[f32]> for GStreamerAudioDecoderProgress {
    fn as_ref(&self) -> &[f32] {
        self.0.as_ref().as_slice_of::<f32>().unwrap()
    }
}

pub struct GStreamerAudioDecoder {}

impl GStreamerAudioDecoder {
    pub fn new() -> Self {
        Self {}
    }
}

impl AudioDecoder for GStreamerAudioDecoder {
    fn decode(
        &self,
        uri: String,
        start_millis: Option<u64>,
        decode_receiver: Arc<Mutex<Receiver<()>>>,
        callbacks: AudioDecoderCallbacks,
        options: Option<AudioDecoderOptions>,
    ) {
        let pipeline = gst::Pipeline::new(None);
        let callbacks = Arc::new(callbacks);

        let decodebin = match gst::ElementFactory::make("uridecodebin", None) {
            Ok(decodebin) => decodebin,
            _ => {
                return callbacks.error(AudioDecoderError::Backend(
                    "uridecodebin creation failed".to_owned(),
                ));
            }
        };

        if let Err(e) = decodebin.set_property("uri", &uri.to_value()) {
            return callbacks.error(AudioDecoderError::Backend(e.to_string()));
        }
        // decodebin uses something called a "sometimes-pad", which is basically
        // a pad that will show up when a certain condition is met,
        // in decodebins case that is media being decoded
        if let Err(e) = pipeline.add_many(&[&decodebin]) {
            return callbacks.error(AudioDecoderError::Backend(e.to_string()));
        }

        if let Err(e) = gst::Element::link_many(&[&decodebin]) {
            return callbacks.error(AudioDecoderError::Backend(e.to_string()));
        }

        let options = options.unwrap_or_default();

        let (sender, receiver) = mpsc::channel();
        let sender = Arc::new(Mutex::new(sender));

        let pipeline_ = pipeline.downgrade();
        let callbacks_ = callbacks.clone();
        let sender_ = sender.clone();
        // Initial pipeline looks like
        //
        //  decodebin2! ...
        //
        // We plug in the second part of the pipeline, including the deinterleave element,
        // once the media starts being decoded.
        decodebin.connect_pad_added(move |_, src_pad| {
            // A decodebin pad was added, if this is an audio file,
            // plug in a deinterleave element to separate each planar channel.
            //
            // Sub pipeline looks like
            //
            // ... decodebin2 ! audioconvert ! audioresample ! capsfilter ! deinterleave ...
            //
            // deinterleave also uses a sometime-pad, so we need to wait until
            // a pad for a planar channel is added to plug in the last part of
            // the pipeline, with the appsink that will be pulling the data from
            // each channel.

            let callbacks = &callbacks_;
            let sender = &sender_;
            let pipeline = match pipeline_.upgrade() {
                Some(pipeline) => pipeline,
                None => {
                    callbacks.error(AudioDecoderError::Backend(
                        "Pipeline failed upgrade".to_owned(),
                    ));
                    let _ = sender.lock().unwrap().send(());
                    return;
                }
            };

            let (is_audio, caps) = {
                let media_type = src_pad.get_current_caps().and_then(|caps| {
                    caps.get_structure(0).map(|s| {
                        let name = s.get_name();
                        (name.starts_with("audio/"), caps.clone())
                    })
                });

                match media_type {
                    None => {
                        callbacks.error(AudioDecoderError::Backend(
                            "Failed to get media type from pad".to_owned(),
                        ));
                        let _ = sender.lock().unwrap().send(());
                        return;
                    }
                    Some(media_type) => media_type,
                }
            };

            if !is_audio {
                callbacks.error(AudioDecoderError::InvalidMediaFormat);
                let _ = sender.lock().unwrap().send(());
                return;
            }

            let sample_audio_info = match gst_audio::AudioInfo::from_caps(&caps) {
                Ok(sample_audio_info) => sample_audio_info,
                _ => {
                    callbacks.error(AudioDecoderError::Backend("AudioInfo failed".to_owned()));
                    let _ = sender.lock().unwrap().send(());
                    return;
                }
            };
            let channels = sample_audio_info.channels();
            callbacks.ready(channels);
            let decode_receiver = decode_receiver.clone();
            let insert_deinterleave = || -> Result<(), AudioDecoderError> {
                let convert = gst::ElementFactory::make("audioconvert", None).map_err(|_| {
                    AudioDecoderError::Backend("audioconvert creation failed".to_owned())
                })?;
                convert
                    .set_property("mix-matrix", &gst::Array::new(&[]).to_value())
                    .expect("mix-matrix property didn't work");
                let resample = gst::ElementFactory::make("audioresample", None).map_err(|_| {
                    AudioDecoderError::Backend("audioresample creation failed".to_owned())
                })?;
                let filter = gst::ElementFactory::make("capsfilter", None).map_err(|_| {
                    AudioDecoderError::Backend("capsfilter creation failed".to_owned())
                })?;
                let deinterleave = gst::ElementFactory::make("deinterleave", Some("deinterleave"))
                    .map_err(|_| {
                        AudioDecoderError::Backend("deinterleave creation failed".to_owned())
                    })?;

                deinterleave
                    .set_property("keep-positions", &true.to_value())
                    .expect("deinterleave doesn't have expected 'keep-positions' property");
                let pipeline_ = pipeline.downgrade();
                let callbacks_ = callbacks.clone();
                deinterleave.connect_pad_added(move |_, src_pad| {
                    // A new pad for a planar channel was added in deinterleave.
                    // Plug in an appsink so we can pull the data from each channel.
                    //
                    // The end of the pipeline looks like:
                    //
                    // ... deinterleave ! queue ! appsink.
                    let callbacks = &callbacks_;
                    let pipeline = match pipeline_.upgrade() {
                        Some(pipeline) => pipeline,
                        None => {
                            return callbacks.error(AudioDecoderError::Backend(
                                "Pipeline failedupgrade".to_owned(),
                            ));
                        }
                    };
                    let insert_sink = || -> Result<(), AudioDecoderError> {
                        let queue = gst::ElementFactory::make("queue", None).map_err(|_| {
                            AudioDecoderError::Backend("queue creation failed".to_owned())
                        })?;
                        let sink = gst::ElementFactory::make("appsink", None).map_err(|_| {
                            AudioDecoderError::Backend("appsink creation failed".to_owned())
                        })?;
                        let appsink = sink.clone().dynamic_cast::<gst_app::AppSink>().unwrap();
                        appsink
                            .set_property("sync", &false.to_value())
                            .expect("appsink doesn't handle expected 'sync' property");
                        appsink
                            .set_property("emit-signals", &true.to_value())
                            .expect("appsink doesn't handle expected 'emit-signals' property");
                        appsink
                            .set_property("max-buffers", &(50 as u32).to_value())
                            .expect("appsink doesn't handle expected 'max-buffers' property");
                        appsink
                            .set_property("wait-on-eos", &true.to_value())
                            .expect("appsink doesn't handle expected 'wait-on-eos' property");

                        let callbacks_ = callbacks.clone();
                        let mut pushed_samples = 0;
                        let decode_receiver = decode_receiver.clone();

                        appsink.set_callbacks(
                            gst_app::AppSinkCallbacks::builder()
                                .new_sample(move |appsink| {
                                    if pushed_samples >= 40 {
                                        decode_receiver.lock().unwrap().recv().unwrap();
                                        pushed_samples = 0;
                                    }

                                    pushed_samples += 1;

                                    let sample =
                                        appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                                    let buffer = sample.get_buffer_owned().ok_or_else(|| {
                                        callbacks_.error(AudioDecoderError::InvalidSample);
                                        gst::FlowError::Error
                                    })?;

                                    let audio_info = sample
                                        .get_caps()
                                        .and_then(|caps| gst_audio::AudioInfo::from_caps(caps).ok())
                                        .ok_or_else(|| {
                                            callbacks_.error(AudioDecoderError::Backend(
                                                "Could not get caps from sample".to_owned(),
                                            ));
                                            gst::FlowError::Error
                                        })?;
                                    let positions = audio_info.positions().ok_or_else(|| {
                                        callbacks_.error(AudioDecoderError::Backend(
                                            "AudioInfo failed".to_owned(),
                                        ));
                                        gst::FlowError::Error
                                    })?;
                                    if let Some(millis) = start_millis {
                                        if buffer.get_pts() < gst::ClockTime::from_mseconds(millis)
                                        {
                                            return Ok(gst::FlowSuccess::Ok);
                                        }
                                    }

                                    for position in positions.iter() {
                                        let buffer = buffer.clone();
                                        let map = if let Ok(map) =
                                            buffer.into_mapped_buffer_readable()
                                        {
                                            map
                                        } else {
                                            callbacks_.error(AudioDecoderError::BufferReadFailed);
                                            return Err(gst::FlowError::Error);
                                        };
                                        let progress = Box::new(GStreamerAudioDecoderProgress(map));
                                        let channel = position.to_mask() as u32;
                                        callbacks_.progress(progress, channel);
                                    }

                                    Ok(gst::FlowSuccess::Ok)
                                })
                                .build(),
                        );

                        let elements = &[&queue, &sink];
                        pipeline
                            .add_many(elements)
                            .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;
                        gst::Element::link_many(elements)
                            .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;

                        for e in elements {
                            e.sync_state_with_parent()
                                .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;
                        }

                        let sink_pad =
                            queue
                                .get_static_pad("sink")
                                .ok_or(AudioDecoderError::Backend(
                                    "Could not get static pad sink".to_owned(),
                                ))?;
                        src_pad.link(&sink_pad).map(|_| ()).map_err(|e| {
                            AudioDecoderError::Backend(format!("Sink pad link failed: {}", e))
                        })
                    };

                    if let Err(e) = insert_sink() {
                        callbacks.error(e);
                    }
                });

                let mut audio_info_builder = gst_audio::AudioInfo::builder(
                    gst_audio::AUDIO_FORMAT_F32,
                    options.sample_rate as u32,
                    channels,
                );
                if let Some(positions) = sample_audio_info.positions() {
                    audio_info_builder = audio_info_builder.positions(positions);
                }
                let audio_info = audio_info_builder
                    .build()
                    .map_err(|_| AudioDecoderError::Backend("AudioInfo failed".to_owned()))?;
                let caps = audio_info
                    .to_caps()
                    .map_err(|_| AudioDecoderError::Backend("AudioInfo failed".to_owned()))?;
                filter
                    .set_property("caps", &caps)
                    .expect("capsfilter doesn't have expected 'caps' property");

                let elements = &[&convert, &resample, &filter, &deinterleave];
                pipeline
                    .add_many(elements)
                    .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;
                gst::Element::link_many(elements)
                    .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;

                for e in elements {
                    e.sync_state_with_parent()
                        .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;
                }

                let sink_pad = convert
                    .get_static_pad("sink")
                    .ok_or(AudioDecoderError::Backend(
                        "Get static pad sink failed".to_owned(),
                    ))?;
                src_pad
                    .link(&sink_pad)
                    .map(|_| ())
                    .map_err(|e| AudioDecoderError::Backend(format!("Sink pad link failed: {}", e)))
            };

            if let Err(e) = insert_deinterleave() {
                callbacks.error(e);
                let _ = sender.lock().unwrap().send(());
            }
        });

        let bus = match pipeline.get_bus() {
            Some(bus) => bus,
            None => {
                callbacks.error(AudioDecoderError::Backend(
                    "Pipeline without bus. Shouldn't happen!".to_owned(),
                ));
                let _ = sender.lock().unwrap().send(());
                return;
            }
        };

        let callbacks_ = callbacks.clone();
        bus.set_sync_handler(move |_, msg| {
            use gst::MessageView;

            match msg.view() {
                MessageView::Error(e) => {
                    callbacks_.error(AudioDecoderError::Backend(
                        e.get_debug().unwrap_or("Unknown".to_owned()),
                    ));
                    let _ = sender.lock().unwrap().send(());
                }
                MessageView::Eos(_) => {
                    callbacks_.eos();
                    let _ = sender.lock().unwrap().send(());
                }
                _ => (),
            }
            gst::BusSyncReply::Drop
        });

        if pipeline.set_state(gst::State::Paused).is_err() {
            callbacks.error(AudioDecoderError::StateChangeFailed);
            return;
        }
        if let Err(_) = pipeline.get_state(gst::ClockTime::none()).0 {
            return callbacks.error(AudioDecoderError::Backend(
                "Error retrieving pipeline state".to_owned(),
            ));
        }
        if let Some(millis) = start_millis {
            if let Err(e) = decodebin.seek_simple(
                gst::SeekFlags::KEY_UNIT | gst::SeekFlags::FLUSH,
                millis * gst::MSECOND,
            ) {
                return callbacks.error(AudioDecoderError::Backend(e.to_string()));
            }
        }
        pipeline.set_state(gst::State::Playing).unwrap();

        // Wait until we get an error or EOS.
        receiver.recv().unwrap();
        let _ = pipeline.set_state(gst::State::Null);
    }
}
