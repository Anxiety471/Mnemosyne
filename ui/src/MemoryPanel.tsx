import { useCallback, useEffect, useState } from "react";
import { api, MemoryDetail, MemoryNote } from "./api";
import { MarkdownView } from "./viewers";
import { MemoryGraph } from "./MemoryGraph";
import { Modal } from "./Modal";

// Obsidian-style display: hide frontmatter (tags show as chips) and turn [[links]] into in-panel links.
const prepare = (text: string) => text
  .replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n?/, "")
  .replace(/\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|([^\]]+))?\]\]/g, (_, target: string, alias?: string) => `[${alias ?? target}](#memory:${encodeURIComponent(target.trim())})`);

export function MemoryPanel({ close }: { close: () => void }) {
  const [notes, setNotes] = useState<MemoryNote[]>([]);
  const [tags, setTags] = useState<{ tag: string; count: number }[]>([]);
  const [query, setQuery] = useState("");
  const [tag, setTag] = useState("");
  const [open, setOpen] = useState<MemoryDetail | null>(null);
  const [error, setError] = useState("");
  const [view, setView] = useState<"list" | "graph">("graph");
  const [draft, setDraft] = useState<{ name: string; content: string; tags: string; existing: boolean } | null>(null);
  const [rev, setRev] = useState(0);
  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  useEffect(() => { api.memoryTags().then(setTags).catch((e) => setError(String(e))); }, [rev]);
  useEffect(() => { let live = true; api.memoryNotes(query, tag).then((n) => { if (live) setNotes(n); }).catch((e) => { if (live) setError(String(e)); }); return () => { live = false; }; }, [query, tag, rev]);
  const show = useCallback((name: string) => { setError(""); api.memoryNote(name).then(setOpen).catch((e) => setError(String(e))); }, []);
  // Stable so MemoryGraph's canvas effect (which lists `open` as a dependency) only restarts
  // when the graph or the filter changes, not on every re-render of this panel.
  const pick = useCallback((name: string, exists: boolean) => { if (exists) show(name); else setDraft({ name, content: "", tags: "", existing: false }); }, [show]);
  const edit = () => open && setDraft({ name: open.name, content: open.content, tags: open.tags.join(", "), existing: true });
  const save = async () => {
    if (!draft) return;
    setSaving(true); setError("");
    try {
      const tagList = draft.tags.split(/[,\s]+/).map((t) => t.replace(/^#/, "")).filter(Boolean);
      const out = await api.saveMemory(draft.name.trim(), draft.content, tagList);
      setDraft(null); setRev((r) => r + 1); show(out.path.replace(/^\/memory\//, "").replace(/\.md$/, ""));
    } catch (e) { setError(String(e)); } finally { setSaving(false); }
  };
  const forget = async () => {
    if (!open) return;
    setConfirmDelete(true);
  };
  const doForget = async () => {
    if (!open) return;
    setConfirmDelete(false);
    try { await api.forgetMemory(open.name); setOpen(null); setRev((r) => r + 1); } catch (e) { setError(String(e)); }
  };
  const onClick = (e: React.MouseEvent) => {
    const href = (e.target as HTMLElement).closest("a")?.getAttribute("href");
    if (href?.startsWith("#memory:")) { e.preventDefault(); show(decodeURIComponent(href.slice(8))); }
  };
  return <>
    <Modal title="Memories" onClose={close} className="operations-modal memory-modal" labelledBy="memory-title">
      <p>Markdown notes in /memory, written by AI agents or by you. Link notes with [[Note Name]] and tag with #tag.</p>
      {error && <p role="alert">{error}</p>}
      {!open && !draft && <div className="memory-tags"><button className="btn memory-tag primary" onClick={() => setDraft({ name: "", content: "", tags: "", existing: false })}>+ New memory</button><button className={`btn memory-tag${view === "graph" ? " primary" : ""}`} onClick={() => setView("graph")}>Graph</button><button className={`btn memory-tag${view === "list" ? " primary" : ""}`} onClick={() => setView("list")}>List</button></div>}
      {draft ? <form className="memory-form" onSubmit={(e) => { e.preventDefault(); void save(); }}>
        <h4>{draft.existing ? `Edit ${draft.name}` : "New memory"}</h4>
        <label>Name<input value={draft.name} readOnly={draft.existing} required placeholder="e.g. Project Drawer or people/Alice" onChange={(e) => setDraft({ ...draft, name: e.target.value })} /></label>
        <label>Note<textarea value={draft.content} required rows={10} placeholder="Markdown. Link other notes with [[Note Name]] and tag with #tag." onChange={(e) => setDraft({ ...draft, content: e.target.value })} /></label>
        <label>Tags<input value={draft.tags} placeholder="project, idea" onChange={(e) => setDraft({ ...draft, tags: e.target.value })} /></label>
        <div className="modal-actions"><button type="button" className="btn" onClick={() => setDraft(null)}>Cancel</button><button type="submit" className="btn primary" disabled={saving}>{saving ? "Saving…" : "Save"}</button></div>
      </form> : open ? <div onClick={onClick}>
        <button className="btn" onClick={() => setOpen(null)}>Back to list</button> <button className="btn" onClick={edit}>Edit</button> <button className="btn danger" onClick={() => void forget()}>Delete</button>
        <h4>{open.name}</h4>
        <div className="memory-tags">{open.tags.map((t) => <button key={t} className="btn memory-tag" onClick={() => { setTag(t); setOpen(null); }}>#{t}</button>)}</div>
        <MarkdownView source={prepare(open.content)} />
        {open.backlinks.length > 0 && <><h4>Linked from</h4><ul className="operations-list">{open.backlinks.map((b) => <li key={b.path}><a href={`#memory:${encodeURIComponent(b.name)}`}>{b.name}</a></li>)}</ul></>}
        <span className="memory-meta">{open.path} · revision {open.revision} · {open.modified_by}</span>
      </div> : view === "graph" ? <>
        <input type="search" placeholder="Filter the graph by name or #tag" aria-label="Filter the graph" value={query} onChange={(e) => setQuery(e.target.value)} />
        <MemoryGraph key={rev} open={pick} filter={query} />
      </> : <>
        <input type="search" placeholder="Search memories" aria-label="Search memories" value={query} onChange={(e) => setQuery(e.target.value)} />
        <div className="memory-tags">{tag && <button className="btn memory-tag" onClick={() => setTag("")}>#{tag} ✕</button>}{!tag && tags.map((t) => <button key={t.tag} className="btn memory-tag" onClick={() => setTag(t.tag)}>#{t.tag} {t.count}</button>)}</div>
        <ul className="operations-list">{notes.map((n) => <li key={n.path}><a href={`#memory:${encodeURIComponent(n.name)}`} onClick={(e) => { e.preventDefault(); show(n.name); }}><strong>{n.name}</strong></a>
          {n.snippet && <span>{n.snippet}</span>}<span>{n.tags.map((t) => `#${t}`).join(" ")} · {n.modified_by} · {new Date(n.modified_at).toLocaleString()}</span></li>)}</ul>
        {!notes.length && <p>No memories yet. Agents save them with the memory_remember tool or `drawer memory remember`.</p>}
      </>}
      {!draft && <div className="modal-actions"><button className="btn" onClick={close}>Done</button></div>}
    </Modal>
    {confirmDelete && open && <Modal title={`Delete "${open.name}"?`} onClose={() => setConfirmDelete(false)} labelledBy="confirm-delete-title">
      <p>It moves to the Trash and can be restored.</p>
      <div className="modal-actions"><button className="btn" onClick={() => setConfirmDelete(false)}>Cancel</button><button className="btn danger" onClick={() => void doForget()}>Delete</button></div>
    </Modal>}
  </>;
}
