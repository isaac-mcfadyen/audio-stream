# audio-stream

A simple daemon used to play audio on another computer's output over the network.

### Features
- Simple, Rust-based daemon
- Compatible with Linux (PulseAudio/ALSA) and macOS (CoreAudio). Windows untested.
- Low latency (real world testing shows 500-800ms end-to-end)
- Allows any sample rate & buffer size supported by both audio devices
- Integrates live terminal-based stats on audio bandwidth and network latency

### Motivation

I was using Airplay to play audio from my MacBook to a Raspberry Pi connected to a speaker.
However, as detailed in [this article](https://darko.audio/2023/10/apple-airplay-isnt-always-lossless-sometimes-its-lossy/),
Airplay has issues in either Airplay 1 or Airplay 2 mode - one mode causes a large audio delay,
and the other encodes audio to 256 kbps AAC.

### Installation
First, [install Rust](https://www.rust-lang.org/tools/install) if you haven't already.

Then install `audio-stream` from GitHub:
```sh
cargo install --git https://github.com/isaac-mcfadyen/audio-stream
```

Finally, if you want to stream the audio output from a macOS source device, install [Blackhole 2ch](https://github.com/ExistentialAudio/BlackHole) and set it as your output in the Audio MIDI Setup application.

### Usage
On the receiving side, run `audio-stream recv`, passing in the device and listen address:
```sh
# Example: receiving and outputting via PulseAudio on Linux
# -d = audio device name
# -l = address to listen on (including port)
audio-stream recv -d pulse -l 0.0.0.0:8000
```

On the sending side, run `audio-stream send`, passing in the device, address of the receiver, and (optionally) any audio parameters:
```sh
# Example: recording and sending audio from Blackhole on macOS
# -d = audio device name
# -c = receiver address (including port)
# -b = buffer size (in frames)
# -r = sample rate (in Hz)
audio-stream send -d "Blackhole 2ch" -c 192.168.0.10:8000 -r 44100 -b 1024
```

### License
Licensed as MIT. See [LICENSE](LICENSE) for details.