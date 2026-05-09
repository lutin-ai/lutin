// Postcard bindings for the chat workflow's plugin protocol. Shared
// between the chat iframe (`workflows/chat/ui`) and the desktop shell
// (`lutin-desktop`) so both can decode `ChatEvent` broadcasts and
// encode `ChatRequest` envelopes off the same wire schema. Authoritative
// source is the Rust types in `workflows/chat/src/lib.rs`.

export * from "./chat";
