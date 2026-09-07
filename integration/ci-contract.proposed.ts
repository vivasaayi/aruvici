// Draft integration DTOs. No API endpoints using these types are implemented yet.
export type BuildProfile =
  | "rust-tauri@1" | "rust-docker@1" | "native-ios@1" | "native-android@1";
export type RunStatus =
  | "queued" | "running" | "blocked" | "passed" | "failed" | "cancelled" | "interrupted";
export type StageStatus = RunStatus | "pending" | "skipped";

export interface StudioContext {
  product_id?: string;
  work_item_id?: string;
  workflow_run_id?: string;
  repository_id: string;
}
export interface BuildRequest {
  target_id: string;
  commit: string;
  idempotency_key: string;
  studio?: StudioContext;
  // No argv, scripts, environment secrets or arbitrary filesystem paths.
}
export interface PipelinePlan {
  target_id: string;
  profile: BuildProfile;
  profile_digest: string;
  config_digest: string;
  approved: boolean;
  readiness_issues: Array<{ code: string; input?: string; message: string }>;
  stages: Array<{
    id: string;
    label: string;
    dependencies: string[];
    required: boolean;
    tool: string;
    resource_class: string;
    timeout_seconds: number;
    produces: string[];
  }>;
}
export interface CiRun {
  id: string;
  target_id: string;
  commit: string;
  profile: BuildProfile;
  profile_digest: string;
  config_digest: string;
  status: RunStatus;
  promotion_eligible: boolean;
  gate_reasons: string[];
  attempt: number;
  created_at: string;
  started_at?: string;
  finished_at?: string;
  studio?: StudioContext;
}
export interface CiEvent {
  schema_version: 1;
  sequence: number;
  run_id: string;
  stage_id?: string;
  occurred_at: string;
  type: "run_status" | "stage_status" | "log_available" | "artifact_added" | "gate_result";
  payload: Record<string, unknown>;
}
export interface CiArtifact {
  id: string;
  run_id: string;
  stage_id: string;
  kind: "package" | "test_report" | "security_report" | "log" | "screenshot" |
    "video" | "symbols" | "sbom" | "manifest";
  name: string;
  content_type: string;
  size_bytes: number;
  sha256: string;
  oci_digest?: string;
  signature: "not_applicable" | "unsigned" | "adhoc_verified" | "developer_id_verified";
  pinned_by_promotion: boolean;
  // Content is fetched by artifact ID; local absolute paths are never API inputs.
}
