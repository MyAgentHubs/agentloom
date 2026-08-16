# fixtures

Cross-endpoint contract fixtures shared by the desktop Rust test suite and the
relay test suite. Each JSON file is a shared source of truth for one slice of
the remote-control wire protocol: wire-frame envelopes, encryption known-answer
vectors, and data-plane milestone frames. `crypto-kat-v1.json` includes the
official RFC 7748 X25519 test vectors.

All values here are public, non-secret protocol fixtures — no live keys,
tokens, or credentials.

The relay server implementation itself is open-sourced separately at
https://github.com/MyAgentHubs/agentloom-remote-control-server
