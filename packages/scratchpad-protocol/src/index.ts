// Postcard bindings for the scratchpad workflow's wire protocol.
// Shared between the chat iframe (`workflows/scratchpad/ui`) and the
// desktop shell so both decode `ChatEvent` broadcasts and encode
// `ChatRequest` envelopes off the same schema. Authoritative source is
// the Rust types in `workflows/scratchpad/src/wire.rs`.

export * from "./chat";
