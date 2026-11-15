# Remote Duo protocol v1

Transport: WebSocket over TLS (`wss://`) on TCP port 47800 (configurable). Every frame is a
JSON text message with a `t` field naming its type. Hosts advertise via Bonjour as
`_remoteduo._tcp` with TXT keys `os` (`macos`/`windows`/`linux`), `v` (protocol version), `ip`,
`port`, and `fp`.

## Trust

Each host makes a self-signed certificate on first run (kept in its config dir as
`identity.der`). Clients don't check it against a CA; they pin its SHA-256 fingerprint
(lowercase hex, `fp`):

- From the pairing QR code's `fp` parameter (best), else the Bonjour TXT `fp`, else trust on
  first use (typed addresses such as Tailscale names).
- Once paired, a client refuses any other certificate for that host. Re-pairing with the PIN
  accepts a new fingerprint (e.g. after reinstalling the host), still subject to the QR/TXT
  fingerprint when one is available.

## Handshake

The client's first frame must be `hello`:

```json
{"t":"hello","name":"James's iPhone","pin":"123456"}      // first time: pair with PIN
{"t":"hello","name":"James's iPhone","token":"9f2c…"}     // later: reconnect with token
```

Host replies with `welcome` (save the token) or `error` and closes:

```json
{"t":"welcome","token":"9f2c…","host":"James's Mac Studio","os":"macos",
 "displays":[{"id":1,"name":"Built-in Display","w":1728,"h":1117,"primary":true}],"fit":true}
{"t":"error","msg":"pairing failed: wrong PIN"}
{"t":"error","msg":"Two devices are already connected…","code":"busy"}
```

An `error` without a `code` means the PIN or token was refused, so ask for a PIN. With
`"code":"busy"`, a PIN won't help: up to two devices can be connected for control at once (a
device reconnecting replaces its own earlier connection), so show the message instead. Video and
audio channels don't count toward the limit.

Clients ping at least every 5 s; the host drops a control connection that's silent for 20 s.

## Client → host

| Message | Meaning |
|---|---|
| `{"t":"move","dx":3.5,"dy":-1.2}` | relative pointer move, points (fractions accumulate) |
| `{"t":"move_abs","x":0.25,"y":0.5,"display":1}` | put the pointer at a spot on a display (default primary), 0–1 from top-left |
| `{"t":"click","b":"left","n":1}` | click; `b` = left/right/middle, `n` = count |
| `{"t":"button","b":"left","down":true}` | press/release (drag) |
| `{"t":"scroll","dx":0,"dy":24}` | scroll in points; +dy reveals content below |
| `{"t":"text","s":"hello"}` | type text |
| `{"t":"key","k":"c","mods":["cmd"]}` | named key or single character with modifiers |
| `{"t":"ping"}` | keepalive; host answers `{"t":"pong"}` |
| `{"t":"fit","w":940,"h":660}` | Fit to Duo: give the computer a screen this size in points (rendered at 2x), mirrored by its real displays |
| `{"t":"unfit"}` | undo `fit` |

Modifiers: `cmd` (⌘ on macOS, Ctrl elsewhere), `ctrl`, `alt`/`option`, `shift`, `meta`/`win`/`super`.

Named keys: `return tab space backspace delete escape up down left right home end page_up
page_down volume_up volume_down mute play_pause next_track prev_track f1`–`f12`.

## Fit to Duo

Only when `welcome` says `"fit":true` (macOS for now). The host answers `fit` and `unfit` with

```json
{"t":"fitted","display":42,"displays":[{"id":42,"name":"Display #1","w":940,"h":660,"primary":true}]}
{"t":"fitted","display":null,"displays":[…],"error":"why it failed"}
```

and the client restarts its video channel on `display` (or its previous display when null).
Sizes below 800×500 points grow to that, keeping their shape; macOS won't run a desktop smaller.
Sending `fit` again with a new size resizes in place. The display goes away on `unfit`, or 30 s
after the last connection that asked for it drops, so a phone reconnecting from the background
doesn't make windows jump around. If the host quits or crashes, macOS removes the display and
undoes the mirroring by itself.

## Video channel (screen view)

Open a second WebSocket to the same port and send a `hello` with the pairing token (PINs are
rejected here), `"channel":"video"`, an optional `max_width` in pixels, and an optional `display`
id from `welcome` (default primary):

```json
{"t":"hello","name":"James's iPhone","token":"9f2c…","channel":"video","max_width":1920,"display":1}
```

After `welcome`, the host streams the main display as H.264 (High profile, no B-frames) in binary
frames, big-endian:

| Packet | Layout |
|---|---|
| config | `0x01` · u16 width · u16 height · u16 len · SPS · u16 len · PPS |
| frame | `0x02` · u8 keyframe (0/1) · u64 pts µs · AVCC NAL units (4-byte lengths) |
| cursor | `0x03` · f32 x · f32 y (0–1 on the display) · u8 draw (1 = video lacks the cursor; client should draw it) |

A config packet precedes every keyframe. Frames only arrive when the screen changes. If the client
loses sync, it sends `{"t":"keyframe"}` and drops frames until the next keyframe. If capture can't
start (e.g. no Screen Recording permission), the host sends `error` and closes.

## Audio channel (computer sound)

Same as the video channel, with `"channel":"audio"`. After `welcome`, the host streams system
sound as raw PCM in binary frames of roughly 10–20 ms:

| Packet | Layout |
|---|---|
| audio | `0x04` · u8 channels · u32 sample rate (big-endian) · s16 little-endian interleaved samples |

Supported on macOS (ScreenCaptureKit, excluding the host's own sound) and Windows (WASAPI
loopback of the default output). Other hosts send `error` and close. The host drops packets
rather than queueing more than ~0.5 s; clients should keep a small jitter buffer and drop when
far behind.

## Files channel (send files)

Same as the video channel, with `"channel":"files"`. After `welcome`, send each file as a text
frame announcing it, then its bytes in binary frames (any size up to a few hundred KB each)
adding up to exactly `size`:

```json
{"t":"file","name":"report.pdf","size":123456}
```

The host saves it in the signed-in user's Downloads folder and answers with the name it used,
or `error` and closes:

```json
{"t":"saved","name":"report (2).pdf"}
```

Only the file name is used (folders are dropped); names Windows can't store are made safe, and
a name already taken gets " (2)", " (3)" and so on. Files are written under a temporary name and
appear only when complete, so a transfer that stops partway leaves nothing behind. Send as many
files as you like, one after another, on the same channel. Windows' sign-in screen (no one
signed in) refuses this channel.

## Pairing link

The host shows a QR code for
`remoteduo://pair?addr=<ip>:<port>&name=<host>&pin=<pin>&fp=<fingerprint>`, which the app handles
to pair without typing.
