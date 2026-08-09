# castavox-core

The parts [Castavox](https://castavox.com) and Pulpitry both need.

Two products present very differently — one is a broadcast studio built on OBS,
the other a presentation app for a projector — but underneath they do several
identical things: capture a microphone, transcribe on the machine, keep a
transcript of what was said, turn a summary into a document a church can send
on, and match paraphrased scripture against an embedding index.

Those parts used to live as copies in two repositories, and copies drift. One
fix — installing a TLS provider before the first HTTPS request — landed in one
product and not the other, where it left speech-model downloads working only by
coincidence for months. This crate exists so that cannot happen again.

## What belongs here

Anything that needs no opinion about its host. Nothing here knows about Tauri,
Qt, OBS, windows, or how settings are stored, because those are exactly the
places the two products genuinely differ. Modules that would need such an
opinion take it as a parameter instead — `log` is the pattern: the crate writes
to whatever sink the host installs.

| Module | What it is |
| --- | --- |
| `audio` | Microphone capture, and the device list |
| `whisper` | Local transcription, and fetching the models it runs on |
| `transcripts` | What was said, kept and searchable |
| `exports` | Markdown to text, Word and PDF, with the formatting intact |
| `embed` | The sentence-embedding model behind paraphrase matching |
| `node` | Whether the machine has a Node runtime the speech bridge can use |
| `tls` | Choosing a TLS backend once, before anything makes a request |
| `log` | Where this crate's diagnostics go |

## Licence

MIT, and it has to be. Castavox is GPL-2.0-or-later, being a fork of OBS.
Pulpitry is proprietary. The same code cannot be GPL in one and closed in the
other: a GPL crate would make Pulpitry GPL the moment it linked, and a
proprietary one could not enter Castavox at all. MIT is absorbed by both, which
is what makes a single shared copy legal rather than merely convenient.

Copyright (c) 2026 Ayorinde G. Smart.
