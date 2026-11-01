// D2 rich references and durable attachments — owned types.
//
// Mirrors `engine/crates/protocol/src/references.rs` (ReferenceMeta, ReferenceProvenance)
// so the wire shape stays a single source of truth on both sides. Image bytes
// never travel through ReferenceMeta — they live in the legacy `Attachment`
// (`config-host.ts`) in-memory only and the chip strip renders the metadata.

import type { ContentTrust } from "./protocol-core.js";

/** Stable identity for a reference. Generated client-side via `crypto.randomUUID()`. */
export type ReferenceId = string;

/** The kind label the chip strip renders and the inline resolver routes on. */
export type ReferenceKind =
  | "external_link"
  | "workspace_file"
  | "generated_document"
  | "image"
  | "artifact"
  | "change_set"
  | "repository"
  | "skill";

export interface GithubRepositoryIdentity {
  owner: string;
  repository: string;
  label: string;
}

/** Recognize only a GitHub repository root, not issues, commits, profiles,
 * look-alike hosts, or unsafe schemes. This lets the transcript upgrade a
 * repository URL without changing ordinary link behavior. */
export function githubRepositoryIdentity(url: string): GithubRepositoryIdentity | null {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return null;
    if (parsed.hostname !== "github.com" && parsed.hostname !== "www.github.com") return null;
    const parts = parsed.pathname.split("/").filter(Boolean);
    if (parts.length !== 2) return null;
    const owner = parts[0];
    const repository = parts[1].replace(/\.git$/i, "");
    const safeSegment = /^[A-Za-z0-9_.-]+$/;
    if (
      !owner ||
      !repository ||
      owner === "." ||
      owner === ".." ||
      repository === "." ||
      repository === ".." ||
      !safeSegment.test(owner) ||
      !safeSegment.test(repository)
    ) {
      return null;
    }
    return { owner, repository, label: `${owner}/${repository}` };
  } catch {
    return null;
  }
}

/** Build the stable metadata object used when a skill is selected from the
 * composer. The id and destination are deterministic so selecting the same
 * skill twice cannot create duplicate chips. */
export function createSkillReference(
  skill: { slug: string; name: string; path?: string },
  createdAt = new Date().toISOString(),
): ReferenceObject | null {
  const slug = skill.slug.trim();
  if (!/^[A-Za-z][A-Za-z0-9_-]{1,63}$/.test(slug)) return null;
  return {
    id: `skill:${slug}`,
    kind: "skill",
    destination: `@${slug}`,
    filename: skill.name.trim() || slug,
    provenance: {
      source: skill.path?.trim() || "composer.skill-picker",
      createdAt,
      trust: "workspaceTrusted",
    },
    actions: [],
  };
}

/** Action affordances the chip / link / viewer expose. The intersection with
 * the renderer's known action set is what actually renders, so a future
 * sender may emit new actions without breaking older renderers. */
export type ReferenceAction = "open" | "reveal" | "copy_path" | "remove" | "preview";

export interface ReferenceProvenance {
  source: string;
  createdAt: string;
  trust: ContentTrust;
  contentHash?: string;
}

/** The full reference as held in memory by the composer / store. Extends the
 * wire-friendly ReferenceMeta with the in-memory `previewUrl` (data: URL for
 * images, http(s) for links) and `error` (per-chip error state, surfaced on
 * restart when image bytes are lost). */
export interface ReferenceObject extends ReferenceMeta {
  /** Ephemeral preview URL — `data:` for image bytes, `http(s):` for links.
   * Never persisted. */
  previewUrl?: string;
  /** Per-chip error message (e.g. "image bytes lost on restart — re-attach"). */
  error?: string;
}

/** The persisted subset — what localStorage stores and what flows across the
 * wire when the engine accepts references. Mirrors the Rust `ReferenceMeta`
 * shape exactly (camelCase). */
export interface ReferenceMeta {
  id: ReferenceId;
  kind: ReferenceKind | string;
  destination: string;
  mime?: string;
  filename?: string;
  sizeBytes?: number;
  provenance: ReferenceProvenance;
  actions?: ReferenceAction[];
}

/** Convert a `ReferenceObject` (in-memory, may have bytes / errors) into the
 * persisted `ReferenceMeta` (wire-friendly, no bytes, no errors). */
export function toReferenceMeta(ref: ReferenceObject): ReferenceMeta {
  const meta: ReferenceMeta = {
    id: ref.id,
    kind: ref.kind,
    destination: ref.destination,
    provenance: ref.provenance,
  };
  if (ref.mime !== undefined) meta.mime = ref.mime;
  if (ref.filename !== undefined) meta.filename = ref.filename;
  if (ref.sizeBytes !== undefined) meta.sizeBytes = ref.sizeBytes;
  if (ref.actions && ref.actions.length > 0) meta.actions = ref.actions;
  return meta;
}

/** Revive a persisted ReferenceMeta into an in-memory ReferenceObject. Sets
 * `error: "image bytes lost on restart — re-attach to resend"` when the
 * persisted record is an image without a recoverable source — the chip
 * surfaces this state to the operator. */
export function fromReferenceMeta(meta: ReferenceMeta): ReferenceObject {
  const ref: ReferenceObject = {
    id: meta.id,
    kind: meta.kind,
    destination: meta.destination,
    provenance: meta.provenance,
    actions: meta.actions ?? [],
  };
  if (meta.mime !== undefined) ref.mime = meta.mime;
  if (meta.filename !== undefined) ref.filename = meta.filename;
  if (meta.sizeBytes !== undefined) ref.sizeBytes = meta.sizeBytes;
  if (meta.kind === "image") {
    // Image bytes never persist — surface the recoverable-error state.
    ref.error = "image bytes lost on restart — re-attach to resend";
  }
  return ref;
}

/** Sentinel for the persisted-meta localStorage key. Versioned so future
 * shape changes can migrate. */
export const REFERENCE_STORAGE_KEY = "cr.references.v1";

/** Hard upper bound on reference count per session — keeps the chip strip
 * bounded and prevents runaway localStorage growth. The composer enforces
 * this in `addReferences`. */
export const REFERENCE_MAX_PER_SESSION = 32;
