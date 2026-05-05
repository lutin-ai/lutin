// Mirrors lutin-control-protocol Rust enums via serde's default JSON
// representation (externally-tagged: unit variants serialize as bare
// strings, struct variants as `{ Variant: { fields } }`).

export type Slug = string;
export type SessionId = string;
export type WorkflowId = string;
export type DisplayName = string;

export interface ProjectInfo {
  slug: Slug;
  display_name: DisplayName;
}

export interface WorkflowInfo {
  id: WorkflowId;
  display_name: string;
  icon: string;
  digest: string;
}

export interface SessionInfo {
  id: SessionId;
  workflow: WorkflowId;
}

export interface SessionEndpoint {
  addr: string;
  token: string;
  project_pubkey: number[];
}

export type Request =
  | "ListProjects"
  | { CreateProject: { slug: Slug; display_name: DisplayName } }
  | { DeleteProject: { slug: Slug } }
  | "ListWorkflows"
  | { ListSessions: { slug: Slug } }
  | { StartSession: { slug: Slug; workflow: WorkflowId } }
  | { StopSession: { slug: Slug; session: SessionId } }
  | { OpenSession: { slug: Slug; session: SessionId } }
  | { GetWorkflowCdylib: { id: WorkflowId } }
  | { GetWorkflowBundle: { id: WorkflowId } };

export type ResponseOk =
  | { Projects: ProjectInfo[] }
  | { Created: ProjectInfo }
  | "Deleted"
  | { Workflows: WorkflowInfo[] }
  | { Sessions: SessionInfo[] }
  | { SessionStarted: { info: SessionInfo; endpoint: SessionEndpoint } }
  | "SessionStopped"
  | { SessionOpened: SessionEndpoint }
  | { WorkflowCdylib: { id: WorkflowId; digest: string; bytes: number[] } }
  | { WorkflowBundle: { id: WorkflowId; digest: string; bytes: number[] } };

export type ApiError =
  | { NotFound: Slug }
  | { AlreadyExists: Slug }
  | { Supervisor: string }
  | { WorkflowNotFound: WorkflowId }
  | { SessionNotFound: SessionId };

export type Response = { Ok: ResponseOk } | { Err: ApiError };

export type CpEvent =
  | { ProjectCreated: ProjectInfo }
  | { ProjectDeleted: { slug: Slug } }
  | { SessionStarted: { slug: Slug; info: SessionInfo } }
  | { SessionEnded: { slug: Slug; session: SessionId } };

export interface ConnectionProfile {
  name: string;
  addr: string;
  token: string;
}

export interface DesktopSettings {
  default: string;
  connections: ConnectionProfile[];
}

// Mirrors Rust `PluginManifest` + `PluginOpened` (lib.rs). `url` is
// the iframe `src`; the React side never constructs it, since the
// custom-protocol URL form differs by platform.
export interface PluginManifest {
  entry: string;
  permissions: string[];
  display_name: string;
  icon: string;
}

export interface PluginOpened {
  url: string;
  manifest: PluginManifest;
}

// Mirrors Rust `ConnSnapshot` (lib.rs) — externally tagged on `kind`,
// lowercase variants. The App store's connection state has the same
// shape, so a `cp_status` invoke can hydrate it directly.
export type ConnState =
  | { kind: "noconfig" }
  | { kind: "connecting" }
  | { kind: "connected" }
  | { kind: "disconnected" }
  | { kind: "rejected"; reason: string }
  | { kind: "error"; error: string };
