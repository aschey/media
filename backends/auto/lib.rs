use platform::servo_media_gstreamer::audio_sink::GStreamerAutoSinkType;
use platform::servo_media_gstreamer::audio_sink::GStreamerSinkType;

#[cfg(any(
    all(
        target_os = "android",
        any(target_arch = "arm", target_arch = "aarch64")
    ),
    target_arch = "x86_64"
))]
mod platform {
    pub extern crate servo_media_gstreamer;
    pub use self::servo_media_gstreamer::GStreamerBackend as Backend;
}

#[cfg(not(any(
    all(
        target_os = "android",
        any(target_arch = "arm", target_arch = "aarch64")
    ),
    target_arch = "x86_64"
)))]
mod platform {
    extern crate servo_media_dummy;
    pub use self::servo_media_dummy::DummyBackend as Backend;
}

pub type Backend = platform::Backend<servo_media_gstreamer::audio_sink::GStreamerAutoSinkType>;
pub type DummyBackend =
    platform::Backend<servo_media_gstreamer::audio_sink::GStreamerDummySinkType>;
