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

### Easy Mode (word codes)

Like magic-wormhole - just share a short code to connect.

**On your local machine (has your monitor):**
```bash
x11q serve
# Prints: x11q join 7-tiger-lamp
```

**On remote machine (runs your apps):**
```bash
x11q join 7-tiger-lamp
# Authenticated via SPAKE2 PAKE
# Creates DISPLAY=:99

DISPLAY=:99 bspwm &
DISPLAY=:99 alacritty
```

The word code is published to mainline DHT (bittorrent) - no central server needed.
Connection is authenticated using SPAKE2 password-authenticated key exchange.

### Direct Mode (node IDs)

For persistent setups or when you want to skip DHT lookup.

**On your local machine:**
```bash
x11q server
# Prints node ID
```

**On remote machine:**
```bash
x11q client NODE_ID
# Creates DISPLAY=:99
```

### Mirror Mode (screen sharing)

**Share your screen:**
```bash
x11q mirror-server
```

**View remote screen:**
```bash
x11q mirror NODE_ID
```

### Show Your Node ID
```bash
x11q id
```

## How It Works

```
Remote: [bspwm] -> DISPLAY=:99 -> x11q join
                                      |
                      (iroh: holepunch via mainline DHT)
                                      |
Local:  [Xorg :0] <- x11q serve <-----+
```

1. `serve` generates word code, publishes node ID to mainline DHT
2. `join` looks up node ID from DHT using word code
3. Both sides perform SPAKE2 key exchange to authenticate
4. iroh establishes P2P connection (holepunch or relay)
5. X11 protocol streams over QUIC
6. Your local GPU renders everything

Result: minimal latency, no compression artifacts, works behind any NAT.

## Security

- Word codes have ~16 bits of entropy (2 words from 256-word list)
- SPAKE2 PAKE prevents MITM even if attacker knows the code
- All traffic encrypted via QUIC/TLS
- DHT records expire after 2 minutes

## Requirements

- Linux with X11
- Rust 1.81+

## License

MIT OR Apache-2.0
