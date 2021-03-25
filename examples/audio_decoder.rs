extern crate bus;
extern crate servo_media;
extern crate servo_media_auto;

use bus::Bus;
use servo_media::audio::context::{AudioContextOptions, RealTimeAudioContextOptions};
use servo_media::audio::decoder::AudioDecoderCallbacks;
use servo_media::audio::node::{AudioNodeInit, AudioNodeMessage, AudioScheduledSourceNodeMessage};
use servo_media::audio::{
    buffer_source_node::{AudioBuffer, AudioBufferSourceNodeMessage},
    gain_node::GainNodeOptions,
};
use servo_media::{ClientContextId, ServoMedia};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::{collections::VecDeque, convert::TryInto};
use std::{env, iter::FromIterator};
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
    let mut default = std::env::current_dir().unwrap();
    default.push("examples/resources/viper_cut.ogg");
    let filename: &str = if args.len() == 2 {
        args[1].as_ref()
    } else if default.exists() {
        default.to_str().unwrap()
    } else {
        panic!("Usage: cargo run --bin audio_decoder <file_path>")
    };

    let buffer_source = context.create_node(
        AudioNodeInit::AudioBufferSourceNode(Default::default()),
        Default::default(),
    );
    let gain = context.create_node(
        AudioNodeInit::GainNode(GainNodeOptions { gain: 1. }),
        Default::default(),
    );
    let dest = context.dest_node();
    context.connect_ports(buffer_source.output(0), gain.input(0));
    context.connect_ports(gain.output(0), dest.input(0));

    let (sender, receiver) = mpsc::channel();
    let (chan_sender, chan_receiver) = mpsc::channel();
    let (data_sender, data_receiver) = mpsc::channel::<(Box<[f32]>, u32)>();
    let mut_sender = Mutex::new(data_sender);

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
    let decode_bus = Arc::new(Mutex::new(Bus::new(16)));

    let uri = glib::filename_to_uri(filename, None).unwrap().to_string();
    context.decode_audio_data(uri, None, decode_bus.clone(), callbacks);
    println!("Decoding audio");

    context.message_node(
        buffer_source,
        AudioNodeMessage::AudioScheduledSourceNode(AudioScheduledSourceNodeMessage::Start(0.)),
    );

    context.message_node(
        buffer_source,
        AudioNodeMessage::AudioBufferSourceNode(AudioBufferSourceNodeMessage::SetNeedDataCallback(
            Box::new(move || {
                decode_bus.lock().unwrap().broadcast(());
            }),
        )),
    );

    let chans = chan_receiver.recv().unwrap();
    let mut decoded_audio = vec![<VecDeque<f32>>::new(); chans];

    let mut set = false;
    while let Ok((data, channel)) = data_receiver.recv() {
        if chans == 0 || data.len() == 0 {
            continue;
        }
        decoded_audio[(channel - 1) as usize].append(&mut VecDeque::from_iter((*data).to_owned()));
        let able_to_push = decoded_audio.iter().map(|a| a.len()).min().unwrap();

        if able_to_push == 0 {
            continue;
        }
        let to_push = decoded_audio
            .iter_mut()
            .map(|a| a.drain(..able_to_push).collect())
            .collect();

        if !set {
            context.message_node(
                buffer_source,
                AudioNodeMessage::AudioBufferSourceNode(AudioBufferSourceNodeMessage::SetBuffer(
                    Some(AudioBuffer::from_buffers(to_push, sample_rate)),
                )),
            );
            set = true;
        } else {
            context.message_node(
                buffer_source,
                AudioNodeMessage::AudioBufferSourceNode(AudioBufferSourceNodeMessage::PushBuffer(
                    to_push,
                )),
            );
        }
    }
    receiver.recv().unwrap();
    println!("Audio decoded");

    thread::sleep(time::Duration::from_millis(5000));

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
