import { uploadChecksum } from "./checksum";
export interface Node {
  id: string;
  parent_id: string | null;
  name: string;
  path: string;
  kind: "file" | "folder";
  owner: string;
  head_revision: number | null;
  hash: string | null;
  size: number | null;
  mime: string | null;
  /** Author of the head revision: who last changed the content. */
  modified_by: string | null;
  properties: Record<string, string>;
  trashed: boolean;
  starred: boolean;
  created_at: string;
  modified_at: string;
}

export interface Revision {
  node_id: string;
  revision: number;
  hash: string;
  size: number;
  mime: string;
  author: string;
  comment: string;
  properties: Record<string, string>;
  created_at: string;
}

export interface WriteOutcome {
  node: Node;
  revision: number;
  new_revision: boolean;
}

export interface UpdateStatus {
  current: string;
  latest: string;
  update_available: boolean;
}

export interface UpdateResult {
  current: string;
  latest: string;
  updated: boolean;
  restart_required: boolean;
}

export interface SearchHit {
  id: string;
  path: string;
  matched: "name" | "content";
  snippet: string;
  line?: number | undefined;
}

export interface DownloadLink {
  max_downloads: number | null;
  downloads: number;
  revision: number | null;
  id: string;
  created_at: string;
  expires_at: string;
}

export interface CreatedDownloadLink extends DownloadLink {
  url_path: string;
  token: string;
}

export interface Diff {
  path: string;
  from: number;
  to: number;
  added: number;
  removed: number;
  diff: string;
}

const BASE = "/drawer/v1";

const TOKEN_KEY = "drawer_token";

function storedToken(): string | null {
  try {
    return localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

/** Remember the bearer token for a server that runs with DRAWER_TOKEN. Empty forgets it. */
export function setToken(token: string) {
  try {
    if (token) localStorage.setItem(TOKEN_KEY, token);
    else localStorage.removeItem(TOKEN_KEY);
  } catch {
    // storage blocked: the token lasts until reload
  }
  memoryToken = token || null;
  if (token) window.dispatchEvent(new Event(AUTHENTICATED_EVENT));
}
let memoryToken: string | null = null;

/** Sent with a percent-encoded comment so the server decodes it instead of reading raw bytes. */
const COMMENT_ENCODING = "x-drawer-comment-encoding";

function commentHeaders(comment: string): Record<string, string> {
  if (/^[\x20-\x7e]*$/.test(comment)) return { "x-drawer-comment": comment };
  return { "x-drawer-comment": encodeURIComponent(comment), [COMMENT_ENCODING]: "percent" };
}


// Optional bearer token, kept in localStorage when the server runs with DRAWER_TOKEN.
function headers(extra: Record<string, string> = {}): Record<string, string> {
  const token = memoryToken ?? storedToken();
  return token ? { ...extra, Authorization: `Bearer ${token}` } : extra;
}

/** Fired on a 401 so the app can ask for the token. */
export const UNAUTHORIZED_EVENT = "drawer:unauthorized";
export const AUTHENTICATED_EVENT = "drawer:authenticated";

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function check(r: Response): Promise<Response> {
  if (r.status === 401) {
    window.dispatchEvent(new Event(UNAUTHORIZED_EVENT));
    throw new ApiError(401, "This Drawer needs an access token");
  }
  if (!r.ok) {
    // The body may be empty, HTML, or JSON with an `error` field: read it as text first.
    const raw = await r.text().catch(() => "");
    let message = `${r.status} ${r.statusText}`;
    try {
      const body = JSON.parse(raw) as { error?: unknown };
      if (typeof body.error === "string" && body.error) message = body.error;
    } catch {
      // not JSON: the status line is the best description
    }
    throw new ApiError(r.status, message);
  }
  return r;
}

const q = (path: string, extra = "") => `path=${encodeURIComponent(path)}${extra}`;
const json = (method: string, body: unknown): RequestInit => ({
  method,
  headers: headers({ "content-type": "application/json" }),
  body: JSON.stringify(body),
});

// ----- Boundary validation -------------------------------------------------------------------
// Every response is checked by hand before it reaches the UI: a server that answers with the
// wrong shape gets a readable error instead of a crash inside a viewer or a table.
const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const str = (v: unknown): string | null => (typeof v === "string" ? v : null);
const bool = (v: unknown): boolean | null => (typeof v === "boolean" ? v : null);

function bad(what: string, missing: string): never {
  throw new ApiError(502, `The server sent an unexpected ${what} (missing ${missing})`);
}

function node(raw: unknown, what = "node"): Node {
  if (!isObj(raw)) return bad(what, "an object");
  const id = str(raw.id), name = str(raw.name), path = str(raw.path), owner = str(raw.owner);
  if (!id || !name || !path || !owner) return bad(what, "id, name, path or owner");
  const kind = str(raw.kind);
  if (kind !== "file" && kind !== "folder") return bad(what, "a file or folder kind");
  const head = raw.head_revision === null ? null : num(raw.head_revision);
  const size = raw.size === null ? null : num(raw.size);
  if ((raw.head_revision !== null && head === null) || (raw.size !== null && size === null)) return bad(what, "head_revision or size");
  return {
    id,
    parent_id: str(raw.parent_id),
    name,
    path,
    kind,
    owner,
    head_revision: head,
    hash: str(raw.hash),
    size,
    mime: str(raw.mime),
    modified_by: str(raw.modified_by),
    properties: isObj(raw.properties) ? (raw.properties as Record<string, string>) : {},
    trashed: bool(raw.trashed) ?? false,
    starred: bool(raw.starred) ?? false,
    created_at: str(raw.created_at) ?? "",
    modified_at: str(raw.modified_at) ?? "",
  };
}

function revision(raw: unknown): Revision {
  if (!isObj(raw)) return bad("revision", "an object");
  const revision = num(raw.revision), size = num(raw.size), author = str(raw.author);
  if (revision === null || size === null || !author) return bad("revision", "revision, size or author");
  return {
    node_id: str(raw.node_id) ?? "",
    revision,
    hash: str(raw.hash) ?? "",
    size,
    mime: str(raw.mime) ?? "",
    author,
    comment: str(raw.comment) ?? "",
    properties: isObj(raw.properties) ? (raw.properties as Record<string, string>) : {},
    created_at: str(raw.created_at) ?? "",
  };
}

/** Reads `{ key: Node[] }`, or an array on its own, and checks every entry. */
function nodeList(value: unknown, what: string, key?: string): Node[] {
  const raw = key === undefined ? value : isObj(value) ? value[key] : null;
  if (!Array.isArray(raw)) return bad(what, key ?? "a list");
  return raw.map((n) => node(n, what));
}
function stringList(value: unknown, what: string): string[] {
  if (!Array.isArray(value) || value.some((s) => typeof s !== "string")) return bad(what, "a list of names");
  return value as string[];
}
function outcome(raw: unknown): WriteOutcome {
  if (!isObj(raw)) return bad("save result", "an object");
  const revision = num(raw.revision);
  const newNode = isObj(raw.node) ? node(raw.node, "saved file") : null;
  if (revision === null || !newNode) return bad("save result", "node or revision");
  return { node: newNode, revision, new_revision: bool(raw.new_revision) ?? false };
}

function diff(raw: unknown): Diff {
  if (!isObj(raw)) return bad("diff", "an object");
  const added = num(raw.added), removed = num(raw.removed), text = str(raw.diff);
  if (added === null || removed === null || text === null) return bad("diff", "added, removed or diff");
  return {
    path: str(raw.path) ?? "",
    from: num(raw.from) ?? 0,
    to: num(raw.to) ?? 0,
    added,
    removed,
    diff: text,
  };
}

function searchHits(value: unknown): SearchHit[] {
  const raw = isObj(value) ? value.hits : null;
  if (!Array.isArray(raw)) return bad("search results", "hits");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("search result", "an object");
    const path = str(entry.path), matched = str(entry.matched), snippet = str(entry.snippet);
    if (!path || (matched !== "name" && matched !== "content") || snippet === null) return bad("search result", "path, matched or snippet");
    const present = entry.line !== undefined && entry.line !== null;
    const line = present ? num(entry.line) : null;
    if (present && line === null) return bad("search result", "line");
    return { id: str(entry.id) ?? path, path, matched, snippet, line: line ?? undefined };
  });
}

function memoryNote(raw: unknown, what: string): MemoryNote {
  if (!isObj(raw)) return bad(what, "an object");
  const name = str(raw.name), path = str(raw.path), modifiedAt = str(raw.modified_at);
  if (!name || !path || !modifiedAt) return bad(what, "name, path or modified_at");
  return {
    name,
    path,
    tags: Array.isArray(raw.tags) ? stringList(raw.tags, what) : [],
    modified_at: modifiedAt,
    modified_by: str(raw.modified_by) ?? "",
    size: num(raw.size) ?? 0,
    snippet: raw.snippet === undefined || raw.snippet === null ? undefined : str(raw.snippet) ?? undefined,
  };
}

function memoryDetail(raw: unknown): MemoryDetail {
  if (!isObj(raw)) return bad("memory note", "an object");
  const content = str(raw.content), modifiedAt = str(raw.modified_at), modifiedBy = str(raw.modified_by);
  if (content === null || !modifiedAt || !modifiedBy) return bad("memory note", "content, modified_at or modified_by");
  const backlinks = Array.isArray(raw.backlinks) ? raw.backlinks.map((entry) => memoryNote(entry, "memory note")) : [];
  const links = Array.isArray(raw.links)
    ? raw.links.map((entry) => {
        if (!isObj(entry) || !str(entry.target)) return bad("memory link", "target");
        const path = str(entry.path);
        return { target: str(entry.target) as string, path: path ?? undefined };
      })
    : [];
  return {
    name: str(raw.name) ?? "",
    path: str(raw.path) ?? "",
    tags: Array.isArray(raw.tags) ? stringList(raw.tags, "memory note") : [],
    modified_at: modifiedAt,
    modified_by: modifiedBy,
    size: num(raw.size) ?? 0,
    snippet: raw.snippet === undefined || raw.snippet === null ? undefined : str(raw.snippet) ?? undefined,
    revision: num(raw.revision) ?? 0,
    content,
    links,
    backlinks,
  };
}

function memoryGraph(raw: unknown): MemoryGraph {
  if (!isObj(raw)) return bad("memory graph", "an object");
  const rawNodes = raw.nodes, rawEdges = raw.edges;
  if (!Array.isArray(rawNodes) || !Array.isArray(rawEdges)) return bad("memory graph", "nodes or edges");
  const nodes = rawNodes.map((entry) => {
    if (!isObj(entry)) return bad("memory graph node", "an object");
    const id = str(entry.id), name = str(entry.name);
    if (!id || !name) return bad("memory graph node", "id or name");
    const path = str(entry.path);
    return {
      id,
      name,
      path: path ?? undefined,
      tags: Array.isArray(entry.tags) ? stringList(entry.tags, "memory graph node") : [],
      out: num(entry.out) ?? 0,
      inbound: num(entry.inbound) ?? 0,
    };
  });
  const edges = rawEdges.flatMap((entry) => {
    if (!Array.isArray(entry) || entry.length !== 2 || entry.some((e) => typeof e !== "string")) return bad("memory graph edge", "a pair of node ids");
    return [[entry[0], entry[1]] as [string, string]];
  });
  return { nodes, edges };
}

function hostMounts(raw: unknown): HostMount[] {
  if (!Array.isArray(raw)) return bad("host folder list", "a list");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("host folder", "an object");
    const id = str(entry.id), name = str(entry.name);
    if (!id || !name) return bad("host folder", "id or name");
    return { id, name };
  });
}

function hostEntries(raw: unknown): HostEntry[] {
  if (!Array.isArray(raw)) return bad("host folder contents", "a list");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("host entry", "an object");
    const name = str(entry.name), path = str(entry.path), kind = str(entry.kind), size = num(entry.size);
    if (!name || !path || (kind !== "file" && kind !== "folder") || size === null) return bad("host entry", "name, path, kind or size");
    return { name, path, kind, size };
  });
}

function activityEvents(raw: unknown): ActivityEvent[] {
  if (!Array.isArray(raw)) return bad("activity history", "a list");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("activity event", "an object");
    const id = num(entry.id), actor = str(entry.actor), method = str(entry.method), route = str(entry.route), status = num(entry.status);
    if (id === null || !actor || !method || !route || status === null) return bad("activity event", "id, actor, method, route or status");
    return { id, actor, method, route, status, created_at: str(entry.created_at) ?? "" };
  });
}

function uploadLinks(raw: unknown): UploadLink[] {
  if (!Array.isArray(raw)) return bad("upload link list", "a list");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("upload link", "an object");
    const id = str(entry.id), expiresAt = str(entry.expires_at);
    const maxFiles = num(entry.max_files), maxBytes = num(entry.max_bytes), files = num(entry.files_uploaded), bytes = num(entry.bytes_uploaded);
    if (!id || !expiresAt || maxFiles === null || maxBytes === null || files === null || bytes === null) return bad("upload link", "id, expires_at or limits");
    return { id, expires_at: expiresAt, max_files: maxFiles, max_bytes: maxBytes, files_uploaded: files, bytes_uploaded: bytes };
  });
}

function users(raw: unknown): UserAccount[] {
  if (!Array.isArray(raw)) return bad("user list", "a list");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("user", "an object");
    const id = str(entry.id), username = str(entry.username);
    const quota = entry.quota_bytes === null ? null : num(entry.quota_bytes);
    if (!id || !username || (entry.quota_bytes !== null && quota === null)) return bad("user", "id, username or quota_bytes");
    return { id, username, quota_bytes: quota ?? 0, disabled: bool(entry.disabled) ?? false };
  });
}

function storageStats(raw: unknown): StorageStats {
  if (!isObj(raw)) return bad("storage stats", "an object");
  // Every field is a byte or file count except quota_bytes, which the server omits when the
  // workspace has no quota. Naming the fields keeps the parser honest: a missing or non-numeric
  // count is reported by name instead of becoming an undefined that renders as "NaN MiB".
  const count = (key: Exclude<keyof StorageStats, "quota_bytes">): number => {
    const value = num(raw[key]);
    if (value === null) return bad("storage stats", key);
    return value;
  };
  return {
    quota_bytes: num(raw.quota_bytes),
    live_files: count("live_files"),
    live_bytes: count("live_bytes"),
    revisions: count("revisions"),
    revision_bytes: count("revision_bytes"),
    blob_bytes: count("blob_bytes"),
    unreferenced_blob_bytes: count("unreferenced_blob_bytes"),
    staging_bytes: count("staging_bytes"),
    database_bytes: count("database_bytes"),
    filesystem_total_bytes: count("filesystem_total_bytes"),
    filesystem_available_bytes: count("filesystem_available_bytes"),
  };
}

function downloadLinks(value: unknown): DownloadLink[] {
  const raw = isObj(value) ? value.links : null;
  if (!Array.isArray(raw)) return bad("download links", "links");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("download link", "an object");
    const id = str(entry.id), createdAt = str(entry.created_at), expiresAt = str(entry.expires_at);
    const downloads = num(entry.downloads);
    if (!id || !createdAt || !expiresAt || downloads === null) return bad("download link", "id, created_at, expires_at or downloads");
    const revision = entry.revision === null ? null : num(entry.revision);
    const max = entry.max_downloads === null ? null : num(entry.max_downloads);
    if ((entry.revision !== null && revision === null) || (entry.max_downloads !== null && max === null)) return bad("download link", "revision or max_downloads");
    return { id, created_at: createdAt, expires_at: expiresAt, downloads, revision, max_downloads: max };
  });
}

function createdLink(raw: unknown): CreatedDownloadLink {
  if (!isObj(raw)) return bad("new download link", "an object");
  const [link] = downloadLinks([raw]);
  const urlPath = str(raw.url_path), token = str(raw.token);
  if (!link) return bad("new download link", "a link");
  if (!urlPath || !token) return bad("new download link", "url_path or token");
  return { ...link, url_path: urlPath, token };
}

function transferRecords(value: unknown): TransferRecord[] {
  const raw = isObj(value) ? value.transfers : null;
  if (!Array.isArray(raw)) return bad("transfers", "transfers");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("transfer", "an object");
    const id = str(entry.id), path = str(entry.path), status = str(entry.status), direction = str(entry.direction);
    const size = num(entry.size), received = num(entry.received);
    if (!id || !path || !status || !direction || size === null || received === null) return bad("transfer", "id, path, status, direction, size or received");
    const error = entry.error === null ? null : str(entry.error);
    if (entry.error !== null && error === null) return bad("transfer", "error");
    return { id, path, status, direction, size, received, error, updated_at: str(entry.updated_at) ?? "" };
  });
}

function permissionsList(value: unknown): Permission[] {
  const raw = isObj(value) ? value.permissions : null;
  if (!Array.isArray(raw)) return bad("permissions", "permissions");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("permission", "an object");
    const principal = str(entry.principal), role = str(entry.role);
    if (!principal || (role !== "reader" && role !== "writer" && role !== "owner")) return bad("permission", "principal or role");
    return { principal, role };
  });
}

function updateStatus(raw: unknown): UpdateStatus {
  if (!isObj(raw)) return bad("update status", "an object");
  const current = str(raw.current), latest = str(raw.latest);
  if (!current || !latest) return bad("update status", "current or latest");
  return { current, latest, update_available: bool(raw.update_available) ?? false };
}

function updateResult(raw: unknown): UpdateResult {
  if (!isObj(raw)) return bad("update result", "an object");
  const current = str(raw.current), latest = str(raw.latest);
  if (!current || !latest) return bad("update result", "current or latest");
  return { current, latest, updated: bool(raw.updated) ?? false, restart_required: bool(raw.restart_required) ?? false };
}

function authStatus(raw: unknown): AuthStatus {
  if (!isObj(raw)) return bad("sign-in status", "an object");
  const authenticated = bool(raw.authenticated);
  if (authenticated === null) return bad("sign-in status", "authenticated");
  return { mode: "password", authenticated, user: str(raw.user) };
}

function tagsList(value: unknown): { tag: string; count: number }[] {
  const raw = isObj(value) ? value.tags : null;
  if (!Array.isArray(raw)) return bad("memory tags", "tags");
  return raw.map((entry) => {
    if (!isObj(entry)) return bad("memory tag", "an object");
    const tag = str(entry.tag), count = num(entry.count);
    if (!tag || count === null) return bad("memory tag", "tag or count");
    return { tag, count };
  });
}

function memorySummaryList(value: unknown, key: string): MemoryNote[] {
  const raw = isObj(value) ? value[key] : null;
  if (!Array.isArray(raw)) return bad("memory notes", key);
  return raw.map((entry) => memoryNote(entry, "memory note"));
}

function memorySaveResult(raw: unknown): { path: string; created: boolean } {
  if (!isObj(raw)) return bad("memory save", "an object");
  const path = str(raw.path);
  if (!path) return bad("memory save", "path");
  return { path, created: bool(raw.created) ?? false };
}

export interface AuthStatus {
  mode: "password";
  authenticated: boolean;
  user: string | null;
}

export interface StorageStats { quota_bytes: number | null; live_files: number; live_bytes: number; revisions: number; revision_bytes: number; blob_bytes: number; unreferenced_blob_bytes: number; staging_bytes: number; database_bytes: number; filesystem_total_bytes: number; filesystem_available_bytes: number; }

export interface HostMount { id: string; name: string }
export interface HostEntry { name: string; path: string; kind: "folder" | "file"; size: number }

export interface ActivityEvent { id: number; actor: string; method: string; route: string; status: number; created_at: string }

export interface UploadLink { id: string; expires_at: string; max_files: number; max_bytes: number; files_uploaded: number; bytes_uploaded: number }

export interface Permission { principal: string; role: "reader" | "writer" | "owner" }

export interface UserAccount { id: string; username: string; quota_bytes: number; disabled: boolean }

export interface MemoryNote { name: string; path: string; tags: string[]; modified_at: string; modified_by: string; size: number; snippet?: string | undefined }
export interface MemoryDetail extends MemoryNote { revision: number; content: string; links: { target: string; path?: string | undefined }[]; backlinks: MemoryNote[] }

export interface GraphNode { id: string; name: string; path?: string | undefined; tags: string[]; out: number; inbound: number }
export interface MemoryGraph { nodes: GraphNode[]; edges: [string, string][] }

export const api = {
  async forgetMemory(name: string): Promise<void> { await check(await fetch(`${BASE}/memory/note?name=${encodeURIComponent(name)}`, { method: "DELETE", headers: headers() })); },
  async saveMemory(name: string, content: string, tags: string[], append = false): Promise<{ path: string; created: boolean }> { const r = await check(await fetch(`${BASE}/memory/note`, json("PUT", { name, content, tags, append }))); return memorySaveResult(await r.json()); },
  async memoryGraph(): Promise<MemoryGraph> { const r = await check(await fetch(`${BASE}/memory/graph`, { headers: headers() })); return memoryGraph(await r.json()); },
  async memoryNotes(q: string, tag: string): Promise<MemoryNote[]> { const p = new URLSearchParams({ limit: "200" }); if (q) p.set("q", q); if (tag) p.set("tag", tag); const r = await check(await fetch(`${BASE}/memory/notes?${p}`, { headers: headers() })); return memorySummaryList(await r.json(), "notes"); },
  async memoryNote(name: string): Promise<MemoryDetail> { const r = await check(await fetch(`${BASE}/memory/note?name=${encodeURIComponent(name)}`, { headers: headers() })); return memoryDetail(await r.json()); },
  async memoryTags(): Promise<{ tag: string; count: number }[]> { const r = await check(await fetch(`${BASE}/memory/tags`, { headers: headers() })); return tagsList(await r.json()); },
  async users(): Promise<UserAccount[]> { const r = await check(await fetch(`${BASE}/users`, { headers: headers() })); return users(await r.json()); },
  async createUser(username: string, password: string, quota_bytes: number): Promise<void> { await check(await fetch(`${BASE}/users`, json("POST", { username, password, quota_bytes }))); },
  async changeUser(id: string, changes: { quota_bytes?: number; disabled?: boolean }): Promise<void> { await check(await fetch(`${BASE}/users/${id}`, json("PATCH", changes))); },
  async uploadLinks(id: string): Promise<UploadLink[]> { const r = await check(await fetch(`${BASE}/nodes/${id}/upload-links`, { headers: headers() })); return uploadLinks(await r.json()); },
  async createUploadLink(id: string, days: number, max_files: number, max_bytes: number): Promise<{ token: string; url_path: string }> { const r = await check(await fetch(`${BASE}/nodes/${id}/upload-links`, json("POST", { days, max_files, max_bytes }))); const body = await r.json(); const token = isObj(body) ? str(body.token) : null; const urlPath = isObj(body) ? str(body.url_path) : null; if (!token || !urlPath) return bad("new upload link", "token or url_path"); return { token, url_path: urlPath }; },
  async revokeUploadLink(id: string, link: string): Promise<void> { await check(await fetch(`${BASE}/nodes/${id}/upload-links/${link}`, { method: "DELETE", headers: headers() })); },
  async activity(before?: number): Promise<ActivityEvent[]> { const r = await check(await fetch(`${BASE}/activity${before ? `?before=${before}` : ""}`, { headers: headers() })); return activityEvents(await r.json()); },
  async mounts(): Promise<HostMount[]> { const r = await check(await fetch(`${BASE}/mounts`, { headers: headers() })); return hostMounts(await r.json()); },
  async hostEntries(id: string, path: string): Promise<HostEntry[]> { const r = await check(await fetch(`${BASE}/mounts/${encodeURIComponent(id)}/entries?path=${encodeURIComponent(path)}`, { headers: headers() })); return hostEntries(await r.json()); },
  async importHost(id: string, path: string, destination: string): Promise<WriteOutcome> { const r = await check(await fetch(`${BASE}/mounts/${encodeURIComponent(id)}/import`, json("POST", { path, destination }))); return outcome(await r.json()); },
  async storage(): Promise<StorageStats> { const r = await check(await fetch(`${BASE}/storage`, { headers: headers() })); return storageStats(await r.json()); },
  /** Is a newer release published than the one this server runs? */
  async updateCheck(): Promise<UpdateStatus> {
    const r = await check(await fetch(`${BASE}/update`, { headers: headers() }));
    return updateStatus(await r.json());
  },
  /** Installs the latest release if it is newer. The server runs the new version after a restart. */
  async updateInstall(): Promise<UpdateResult> {
    const r = await check(await fetch(`${BASE}/update`, { method: "POST", headers: headers() }));
    return updateResult(await r.json());
  },
  async authStatus(): Promise<AuthStatus> {
    const response = await fetch("/auth/me", { credentials: "same-origin" });
    if (!response.ok) throw new ApiError(response.status, "Password authentication is not configured");
    return authStatus(await response.json());
  },
  async login(password: string, username = "admin"): Promise<void> {
    const response = await fetch("/auth/login", {
      method: "POST", credentials: "same-origin",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ username, password }),
    });
    if (!response.ok) throw new ApiError(response.status, "Incorrect username or password");
    window.dispatchEvent(new Event(AUTHENTICATED_EVENT));
  },
  async logout(): Promise<void> {
    await check(await fetch("/auth/logout", { method: "POST", credentials: "same-origin" }));
  },
  async downloadArchive(ids: string[]): Promise<void> {
    const url = `${BASE}/archive?ids=${encodeURIComponent(ids.join(","))}`;
    const status = await api.authStatus().catch(() => null);
    if (status?.authenticated) {
      const a = document.createElement("a"); a.href = url; a.download = "selection.zip"; a.click();
    } else {
      const response = await check(await fetch(url, { headers: headers() }));
      const objectUrl = URL.createObjectURL(await response.blob());
      const a = document.createElement("a"); a.href = objectUrl; a.download = "selection.zip"; a.click();
      setTimeout(() => URL.revokeObjectURL(objectUrl), 10000);
    }
  },
  async tree(path: string): Promise<Node[]> {
    const r = await check(await fetch(`${BASE}/tree?${q(path)}`, { headers: headers() }));
    return nodeList(await r.json(), "folder listing", "entries");
  },
  async stat(path: string): Promise<Node> {
    const r = await check(await fetch(`${BASE}/files/meta?${q(path)}`, { headers: headers() }));
    return node(await r.json(), "file details");
  },
  async mkdir(path: string): Promise<void> {
    await check(await fetch(`${BASE}/folders`, json("POST", { path })));
  },
  // Raw bytes of a revision (the head by default), for viewers and downloads.
  async readBlob(path: string, revision?: number, download = false): Promise<Blob> {
    const extra = (revision ? `&revision=${revision}` : "") + (download ? "&download=true" : "");
    const r = await check(await fetch(`${BASE}/files?${q(path, extra)}`, { headers: headers() }));
    return r.blob();
  },
  // Saves the file through the browser's download prompt.
  async download(path: string, revision?: number): Promise<void> {
    // The browser's native download manager streams files without buffering multi-GB
    // content in JavaScript memory. HttpOnly sessions travel with same-origin requests.
    const status = await api.authStatus().catch(() => null);
    if (status?.authenticated) {
      const extra = (revision ? `&revision=${revision}` : "") + "&download=true";
      const a = document.createElement("a");
      a.href = `${BASE}/files?${q(path, extra)}`;
      a.download = path.split("/").pop() || "download";
      a.click();
      return;
    }
    // Legacy DRAWER_TOKEN mode needs an Authorization header and therefore a fetch/blob.
    const blob = await api.readBlob(path, revision, true);
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    const name = path.split("/").pop() ?? "download";
    const dot = name.lastIndexOf(".");
    a.download = revision ? `${dot > 0 ? name.slice(0, dot) : name} (revision ${revision})${dot > 0 ? name.slice(dot) : ""}` : name;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
  },
  // content is text or a File/Blob for uploads.
  // ifHeadRevision: 0 = create only; N = only if the head is still N. A stale save gets a 412.
  async write(path: string, content: string | Blob, comment: string, ifHeadRevision?: number): Promise<WriteOutcome> {
    const guard = ifHeadRevision === undefined ? "" : `&if_head_revision=${ifHeadRevision}`;
    const r = await check(
      await fetch(`${BASE}/files?${q(path, guard)}`, {
        method: "PUT",
        // Header values must be ASCII; non-ASCII comments are percent-encoded and tagged.
        headers: headers({ "x-drawer-author": "ui", ...commentHeaders(comment) }),
        body: content,
      }),
    );
    return outcome(await r.json());
  },
  /** Upload in 4 MiB chunks; restart the same selected File to resume a failed transfer. */
  async upload(
    path: string,
    file: File,
    onProgress: (done: number, total: number, resumed: number) => void,
    signal?: AbortSignal,
    transferId?: string,
  ): Promise<WriteOutcome> {
    onProgress(0, file.size, 0);
    const sha256 = await uploadChecksum(file, signal);
    const key = `drawer:transfer:v2:${path}:${sha256}`;
    const opts = { headers: headers() };
    let id: string | null = transferId ?? null;
    try { id ??= localStorage.getItem(key); } catch { /* Storage may be blocked. */ }
    let offset = 0;
    if (id) {
      try {
        const r = await check(await fetch(`${BASE}/transfers/${encodeURIComponent(id)}`, opts));
        const state = await r.json();
        if (!isObj(state)) throw new Error("Upload changed");
        const expected = num(state.size) === file.size && str(state.path) === path && str(state.sha256) === sha256;
        if (!expected) throw new Error("Upload changed");
        offset = Math.max(0, num(state.received) ?? 0);
      } catch (error) {
        if (transferId || !(error instanceof ApiError) || error.status !== 404) throw error;
        try { localStorage.removeItem(key); } catch { /* Optional resume cache. */ }
        id = null;
      }
    }
    if (!id) {
      const r = await check(await fetch(`${BASE}/transfers`, json("POST", { path, size: file.size, sha256 })));
      const state = await r.json();
      const newId = isObj(state) ? str(state.id) : null;
      if (!newId) return bad("transfer id", "id");
      id = newId;
      try { localStorage.setItem(key, id); } catch { /* Uploads still work without resume caching. */ }
    }
    await api.changeTransfer(id, "active");
    try {
    onProgress(offset, file.size, offset);
    while (offset < file.size) {
      if (signal?.aborted) throw new DOMException("Upload paused", "AbortError");
      const chunk = file.slice(offset, Math.min(offset + 4 * 1024 * 1024, file.size));
      const r = await check(await fetch(`${BASE}/transfers/${encodeURIComponent(id)}`, {
        method: "PUT",
        headers: headers({ "x-upload-offset": String(offset) }),
        body: chunk,
        ...(signal ? { signal } : {}),
      }));
      const state = await r.json();
      const received = isObj(state) ? num(state.received) : null;
      if (received === null || received <= offset) throw new Error("Transfer made no progress");
      offset = received;
      onProgress(offset, file.size, 0);
    }
    const r = await check(await fetch(`${BASE}/transfers/${encodeURIComponent(id)}/complete`, {
      method: "POST", headers: headers(), ...(signal ? { signal } : {}),
    }));
    const saved = outcome(await r.json());
    if (saved.node.hash !== sha256) throw new Error("Server returned a different file checksum");
    try { localStorage.removeItem(key); } catch { /* Optional resume cache. */ }
    return saved;
    } catch (error) {
      if (signal?.aborted) await api.changeTransfer(id, "paused").catch(() => {});
      throw error;
    }
  },
  async transfers(): Promise<TransferRecord[]> {
    const r = await check(await fetch(`${BASE}/transfers`, { headers: headers() }));
    return transferRecords(await r.json());
  },
  async changeTransfer(id: string, status: "paused" | "active"): Promise<void> {
    await check(await fetch(`${BASE}/transfers/${encodeURIComponent(id)}`, json("PATCH", { status })));
  },
  async cancelTransfer(id: string): Promise<void> {
    await check(await fetch(`${BASE}/transfers/${encodeURIComponent(id)}`, { method: "DELETE", headers: headers() }));
  },
  // Moves the file to the trash. Restore it from the Trash view.
  async trash(path: string): Promise<void> {
    await check(await fetch(`${BASE}/files?${q(path)}`, { method: "DELETE", headers: headers() }));
  },
  async revisions(path: string): Promise<Revision[]> {
    const r = await check(await fetch(`${BASE}/revisions?${q(path)}`, { headers: headers() }));
    const body = await r.json();
    const raw = isObj(body) ? body.revisions : null;
    if (!Array.isArray(raw)) return bad("revision history", "revisions");
    return raw.map((entry) => revision(entry));
  },
  async rollback(path: string, revision: number): Promise<void> {
    await check(await fetch(`${BASE}/rollback`, json("POST", { path, revision, author: "ui" })));
  },
  async update(id: string, patch: { name?: string; parent_id?: string; starred?: boolean; trashed?: boolean }): Promise<Node> {
    const r = await check(
      await fetch(`${BASE}/nodes/${encodeURIComponent(id)}`, {
        ...json("PATCH", patch),
        headers: headers({ "content-type": "application/json", "x-drawer-author": "ui" }),
      }),
    );
    return node(await r.json(), "updated node");
  },
  // Permanent: the node must already be in the trash.
  async purge(id: string): Promise<void> {
    await check(
      await fetch(`${BASE}/nodes/${encodeURIComponent(id)}`, {
        method: "DELETE",
        headers: headers({ "x-drawer-author": "ui" }),
      }),
    );
  },
  async starred(): Promise<Node[]> {
    const r = await check(await fetch(`${BASE}/starred`, { headers: headers() }));
    return nodeList(await r.json(), "starred files", "nodes");
  },
  async recent(limit = 50): Promise<Node[]> {
    const r = await check(await fetch(`${BASE}/recent?limit=${limit}`, { headers: headers() }));
    return nodeList(await r.json(), "recent files", "nodes");
  },
  async diff(path: string, from?: number, to?: number): Promise<Diff> {
    const extra = (from !== undefined ? `&from=${from}` : "") + (to !== undefined ? `&to=${to}` : "");
    const r = await check(await fetch(`${BASE}/diff?${q(path, extra)}`, { headers: headers() }));
    return diff(await r.json());
  },
  // Rename or move by path, keeping the id and history. Missing folders are created.
  async move(id: string, to: string): Promise<Node> {
    const r = await check(
      await fetch(`${BASE}/move`, { ...json("POST", { from_id: id, to }), headers: headers({ "content-type": "application/json", "x-drawer-author": "ui" }) }),
    );
    return node(await r.json(), "moved node");
  },
  async copy(id: string, to: string): Promise<WriteOutcome> {
    const r = await check(await fetch(`${BASE}/copy`, json("POST", { from_id: id, to, author: "ui", if_head_revision: 0 })));
    return outcome(await r.json());
  },
  async links(id: string): Promise<DownloadLink[]> {
    const r = await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/links`, { headers: headers() }));
    return downloadLinks(await r.json());
  },
  async createLink(id: string, days = 7, revision?: number, maxDownloads?: number): Promise<CreatedDownloadLink> {
    const r = await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/links`, json("POST", { expires_in_days: days, revision, max_downloads: maxDownloads })));
    return createdLink(await r.json());
  },
  // Who else can reach this node. A node without grants is open to every actor.
  async permissions(id: string): Promise<Permission[]> {
    const r = await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/permissions`, { headers: headers() }));
    return permissionsList(await r.json());
  },
  async setPermission(id: string, principal: string, role: Permission["role"]): Promise<Permission[]> {
    const r = await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/permissions`, json("PUT", { principal, role })));
    return permissionsList(await r.json());
  },
  async removePermission(id: string, principal: string): Promise<Permission[]> {
    const r = await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/permissions/${encodeURIComponent(principal)}`, { method: "DELETE", headers: headers() }));
    return permissionsList(await r.json());
  },
  async revokeLink(id: string, linkId: string): Promise<void> {
    await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/links/${encodeURIComponent(linkId)}`, { method: "DELETE", headers: headers() }));
  },
  async trashed(): Promise<Node[]> {
    const r = await check(await fetch(`${BASE}/trash`, { headers: headers() }));
    return nodeList(await r.json(), "trashed files", "nodes");
  },
  async search(query: string): Promise<SearchHit[]> {
    const r = await check(await fetch(`${BASE}/search?q=${encodeURIComponent(query)}`, { headers: headers() }));
    return searchHits(await r.json());
  },
};

export interface TransferRecord { id: string; path: string; size: number; received: number; status: string; direction: string; error: string | null; updated_at: string }
