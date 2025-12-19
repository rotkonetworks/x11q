# x11quic

X11 display forwarding over QUIC - lowest latency remote desktop.

No video encoding. Your local GPU renders everything. Just raw X11 protocol over QUIC.

## Install

```bash
cargo install x11quic
```

Or build from source:
```bash
git clone https://github.com/rotkonetworks/x11quic
cd x11quic
cargo build --release
```

## Usage

### Reverse mode (you're behind NAT, remote has public IP)

**Remote machine (has public IP, runs your apps):**
```bash
x11quic rserver -b YOUR_PUBLIC_IP:5000
# Note the peer ID printed

# Then start your window manager:
DISPLAY=:99 bspwm
```

**Local machine (behind NAT, has your monitor):**
```bash
x11quic rclient PEERID@REMOTE_IP:5000 -d :0
```

### Normal mode (you have public IP)

**Local machine (has public IP and monitor):**
```bash
x11quic server -d :0
# Note the peer ID printed
```

**Remote machine (runs your apps):**
```bash
x11quic client PEERID@LOCAL_IP:5000
# Creates DISPLAY=:99

DISPLAY=:99 bspwm
```

### Show peer ID
```bash
x11quic id
```

## How it works

```
Remote: [bspwm] -> DISPLAY=:99 -> x11quic -> QUIC/UDP -> x11quic -> [local Xorg]
```

X11 protocol is forwarded over QUIC (UDP-based). Your local X server and GPU do all the rendering. Result: minimal latency, no compression artifacts.

## Requirements

- Linux with X11
- Rust 1.70+

## License

MIT OR Apache-2.0
