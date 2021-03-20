extern crate servo_media;
extern crate servo_media_auto;

use servo_media::audio::buffer_source_node::{AudioBuffer, AudioBufferSourceNodeMessage};
use servo_media::audio::context::{AudioContextOptions, RealTimeAudioContextOptions};
use servo_media::audio::decoder::AudioDecoderCallbacks;
use servo_media::audio::node::{AudioNodeInit, AudioNodeMessage, AudioScheduledSourceNodeMessage};
use servo_media::{ClientContextId, ServoMedia};
use std::env;
use std::fs::File;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::{convert::TryInto, io::Read};
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
    let default = "./examples/resources/viper_cut.ogg";
    let filename: &str = if args.len() == 2 {
        args[1].as_ref()
    } else if Path::new(default).exists() {
        default
    } else {
        panic!("Usage: cargo run --bin audio_decoder <file_path>")
    };
    let mut file = File::open(filename).unwrap();
    let mut bytes = vec![];
    file.read_to_end(&mut bytes).unwrap();
    let buffer_source = context.create_node(
        AudioNodeInit::AudioBufferSourceNode(Default::default()),
        Default::default(),
    );
    let dest = context.dest_node();
    context.connect_ports(buffer_source.output(0), dest.input(0));

    let (sender, receiver) = mpsc::channel();
    let (data_sender, data_receiver) = mpsc::channel::<(Box<[f32]>, u32)>();
    let mut_sender = Mutex::new(data_sender);

    let mut decoded_audio = vec![<Vec<f32>>::new()];

    let callbacks = AudioDecoderCallbacks::new()
        .eos(move || {
            sender.send(()).unwrap();
        })
        .error(|e| {
            eprintln!("Error decoding audio {:?}", e);
        })
        .progress(move |buffer, channel| {
            let r = (*buffer).as_ref().try_into().unwrap();
            mut_sender.lock().unwrap().send((r, channel)).unwrap();
        })
        .ready(move |channels| {
            println!("There are {:?} audio channels", channels);
            decoded_audio.resize(channels as usize, Vec::new());
        })
        .build();
    context.decode_audio_data(bytes.to_vec(), callbacks);
    println!("Decoding audio");
    context.message_node(
        buffer_source,
        AudioNodeMessage::AudioScheduledSourceNode(AudioScheduledSourceNodeMessage::Start(0.)),
    );

    let mut set = false;
    let mut decoded_audio = vec![<Vec<f32>>::new(); 2];
    'outer: while let Ok((data, channel)) = data_receiver.recv() {
        let chans = decoded_audio.len();
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
