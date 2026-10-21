import { useEffect, useState } from "react";
import { api, HostEntry, HostMount } from "./api";
import { Modal } from "./Modal";

export function MountsPanel({ close, currentDir }: { close: () => void; currentDir: string }) {
  const [mounts, setMounts] = useState<HostMount[]>([]);
  const [mount, setMount] = useState("");
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<HostEntry[]>([]);
  const [selected, setSelected] = useState<HostEntry | null>(null);
  const [destination, setDestination] = useState("");
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => { let active = true; api.mounts().then((items) => { if (active) { setMounts(items); setMount(items[0]?.id ?? ""); } }).catch((e) => { if (active) setError(String(e)); }); return () => { active = false; }; }, []);
  useEffect(() => {
    let active = true; setEntries([]); setSelected(null); setError("");
    if (mount) api.hostEntries(mount, path).then((items) => { if (active) setEntries(items); }).catch((e) => { if (active) setError(String(e)); });
    return () => { active = false; };
  }, [mount, path]);
  const importFile = async () => {
    if (!selected) return;
    setBusy(true); setError(""); setMessage("");
    try { const result = await api.importHost(mount, selected.path, destination); setMessage(`Imported ${result.node.path} as revision ${result.revision}.`); setSelected(null); }
    catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const segments = path.replace(/^\.\//, "").split("/").filter(Boolean);
  const upPath = segments.length <= 1 ? "" : segments.slice(0, -1).join("/");
  return <Modal title="Host folders" onClose={close} className="operations-modal" labelledBy="mounts-title">
    <p>Browse registered server folders and copy files into Drawer. Host files remain read-only. Each import is limited to 512 MiB.</p>
    {!mounts.length && <p>No host folders registered. On the server, run <code>drawer mount add NAME /absolute/directory</code> using the same store.</p>}
    {!!mounts.length && <><label>Host folder <select aria-label="Host folder" disabled={busy} value={mount} onChange={(e) => { setMount(e.target.value); setPath(""); }}>{mounts.map((m) => <option key={m.id} value={m.id}>{m.name}</option>)}</select></label>
      <div className="operations-actions"><button className="btn" disabled={!path || busy} onClick={() => setPath(upPath)}>Up one folder</button>
        <div className="memory-tags">{segments.map((seg, i) => <button key={i} className="btn memory-tag" onClick={() => setPath(segments.slice(0, i + 1).join("/"))}>{seg}</button>)}</div>
      </div>
      <ul className="operations-list">{entries.map((entry) => <li key={entry.path}><button className="btn" disabled={busy} onClick={() => { if (entry.kind === "folder") setPath(entry.path.replace(/^\.\//, "")); else { setSelected(entry); setDestination(currentDir === "/" ? `/${entry.name}` : `${currentDir}/${entry.name}`); setMessage(""); } }}>{entry.name}{entry.kind === "folder" ? "/" : ` · ${entry.size.toLocaleString()} bytes`}</button></li>)}</ul>
    </>}
    {selected && <div><p>Import {selected.name}</p><label>Drawer destination <input aria-label="Drawer destination" disabled={busy} value={destination} onChange={(e) => setDestination(e.target.value)} placeholder="/folder/file.txt" /></label><p className="muted small">Defaults to the folder you are browsing ({currentDir}).</p><button className="btn primary" disabled={busy || !destination.startsWith("/")} onClick={() => void importFile()}>{busy ? "Importing…" : "Import file"}</button><p>Existing files are never overwritten.</p></div>}
    {error && <p role="alert">{error}</p>}{message && <p role="status">{message}</p>}
    <div className="modal-actions"><button className="btn" disabled={busy} onClick={close}>Done</button></div>
  </Modal>;
}
