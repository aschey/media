extern crate servo_media;
extern crate servo_media_auto;

use servo_media::audio::buffer_source_node::{AudioBuffer, AudioBufferSourceNodeMessage};
use servo_media::audio::context::{AudioContextOptions, RealTimeAudioContextOptions};
use servo_media::audio::decoder::AudioDecoderCallbacks;
use servo_media::audio::node::{AudioNodeInit, AudioNodeMessage, AudioScheduledSourceNodeMessage};
use servo_media::{ClientContextId, ServoMedia};
use std::fs::File;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::{convert::TryInto, io::Read};
use std::{env, io::BufReader};
use std::{thread, time};

fn run_example(servo_media: Arc<ServoMedia>) {
    let options = <RealTimeAudioContextOptions>::default();
    let sample_rate = options.sample_rate;
    let context = servo_media.create_audio_context(
        &ClientContextId::build(1, 1),
        AudioContextOptions::RealTimeAudioContext(options),
    );

    let context = context.lock().unwrap();
    let _ = context.resume();
    let args: Vec<_> = env::args().collect();
    let default = "./examples/resources/viper_cut.ogg"; //"C:\\shared_files\\Music\\EDM Mixes\\April - 2013.mp3";
    let filename: &str = if args.len() == 2 {
        args[1].as_ref()
    } else if Path::new(default).exists() {
        default
    } else {
        panic!("Usage: cargo run --bin audio_decoder <file_path>")
    };

    let buffer_source = context.create_node(
        AudioNodeInit::AudioBufferSourceNode(Default::default()),
        Default::default(),
    );
    let dest = context.dest_node();
    context.connect_ports(buffer_source.output(0), dest.input(0));

    let (sender, receiver) = mpsc::channel();
    let (chan_sender, chan_receiver) = mpsc::channel();
    let (data_sender, data_receiver) = mpsc::channel::<(Box<[f32]>, u32)>();
    let mut_sender = Mutex::new(data_sender);

    let file = File::open(filename).unwrap();
    let reader = Mutex::new(BufReader::new(file));

    let callbacks = AudioDecoderCallbacks::new()
        .eos(move || {
            sender.send(()).unwrap();
        })
        .error(|e| {
            eprintln!("Error decoding audio {:?}", e);
        })
        .progress(move |buffer, channel| {
            let buf = (*buffer).as_ref().try_into().unwrap();
            mut_sender.lock().unwrap().send((buf, channel)).unwrap();
        })
        .ready(move |channels| {
            println!("There are {:?} audio channels", channels);
            chan_sender.send(channels as usize).unwrap();
        })
        .build();
    let (decode_sender, decode_receiver) = mpsc::channel();
    context.decode_audio_data(callbacks, decode_receiver);
    println!("Decoding audio");

    context.message_node(
        buffer_source,
        AudioNodeMessage::AudioScheduledSourceNode(AudioScheduledSourceNodeMessage::Start(0.)),
    );

    context.message_node(
        buffer_source,
        AudioNodeMessage::AudioBufferSourceNode(AudioBufferSourceNodeMessage::SetNeedDataCallback(
            Box::new(move |buffer_size| {
                let mut buffer = vec![0; buffer_size];
                match reader.lock().unwrap().read(&mut buffer) {
                    Ok(0) => {
                        decode_sender.send(vec![]).unwrap_or_default();
                    }
                    Ok(size) => {
                        decode_sender
                            .send(buffer[..size].to_vec())
                            .unwrap_or_default();
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        decode_sender.send(vec![]).unwrap_or_default();
                    }
                }
            }),
        )),
    );

    let chans = chan_receiver.recv().unwrap();
    let mut decoded_audio = vec![<Vec<f32>>::new(); chans];

    let mut set = false;
    'outer: while let Ok((data, channel)) = data_receiver.recv() {
        if chans == 0 || data.len() == 0 {
            continue;
        }
        decoded_audio[(channel - 1) as usize].extend_from_slice((*data).as_ref());

        for chan in &decoded_audio {
            if decoded_audio[0].len() != chan.len() {
                continue 'outer;
            }
        }

        if !set {
            context.message_node(
                buffer_source,
                AudioNodeMessage::AudioBufferSourceNode(AudioBufferSourceNodeMessage::SetBuffer(
                    Some(AudioBuffer::from_buffers(
                        decoded_audio.to_vec(),
                        sample_rate,
                    )),
                )),
            );
            set = true;
        } else {
            context.message_node(
                buffer_source,
                AudioNodeMessage::AudioBufferSourceNode(AudioBufferSourceNodeMessage::PushBuffer(
                    decoded_audio.to_vec(),
                )),
            );
        }

        decoded_audio = vec![<Vec<f32>>::new(); chans];
    }
    receiver.recv().unwrap();
    println!("Audio decoded");

    thread::sleep(time::Duration::from_millis(6000));
    let _ = context.close();
}

fn main() {
    ServoMedia::init::<servo_media_auto::Backend>();
    if let Ok(servo_media) = ServoMedia::get() {
        run_example(servo_media);
    } else {
        unreachable!()
    }
}
