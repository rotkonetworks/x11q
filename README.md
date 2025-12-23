# x11q

X11 display forwarding over QUIC with P2P holepunching - lowest latency remote desktop.

No video encoding. Your local GPU renders everything. Just raw X11 protocol over QUIC.

Built on [iroh](https://iroh.computer/) for automatic NAT traversal - works behind any NAT without port forwarding.

## Install

```bash
cargo install x11q
```

Or build from source:
```bash
git clone https://github.com/rotkonetworks/x11q
cd x11q
cargo build --release
```

## Usage

### X11 Forwarding (run apps remotely, render locally)

**On your local machine (has your monitor):**
```bash
x11q server
# Prints node ID and connects to relay
```

**On remote machine (runs your apps):**
```bash
x11q client NODE_ID
# Creates DISPLAY=:99

DISPLAY=:99 bspwm &
DISPLAY=:99 alacritty
```

No port forwarding needed - iroh handles NAT traversal automatically via holepunching. Falls back to relay if direct connection fails.

### Mirror Mode (screen sharing)

**Share your screen:**
```bash
x11q mirror-server
# Prints node ID
```

**View remote screen:**
```bash
x11q mirror NODE_ID
```

### Show Your Node ID
```bash
x11q id
```

### Optional: Direct Address Hint

If you know the remote IP, provide it for faster connection:
```bash
x11q client NODE_ID --addr 192.168.1.100:5000
```

## How It Works

```
Remote: [bspwm] -> DISPLAY=:99 -> x11q client
                                      |
                          (iroh: holepunch or relay)
                                      |
Local:  [Xorg :0] <- x11q server <----+
```

1. Both sides connect to iroh relay network
2. iroh attempts UDP holepunch for direct P2P
3. Falls back to relay if holepunch fails
4. X11 protocol streams over QUIC
5. Your local GPU renders everything

Result: minimal latency, no compression artifacts, works behind any NAT.

## Requirements

- Linux with X11
- Rust 1.70+

## License

MIT OR Apache-2.0
