import { useEffect, useState } from "react";
import { api, Node, UploadLink } from "./api";
import { Modal } from "./Modal";

export function UploadLinksDialog({ node, close }: { node: Node; close: () => void }) {
  const [links, setLinks] = useState<UploadLink[]>([]);
  const [url, setUrl] = useState("");
  const [days, setDays] = useState(7);
  const [files, setFiles] = useState(100);
  const [mib, setMib] = useState(1024);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => { let active = true; api.uploadLinks(node.id).then((items) => { if (active) setLinks(items); }).catch((e) => { if (active) setError(String(e)); }); return () => { active = false; }; }, [node.id]);
  const perform = async (action: () => Promise<void>) => { setBusy(true); setError(""); try { await action(); setLinks(await api.uploadLinks(node.id)); } catch (e) { setError(String(e)); } finally { setBusy(false); } };
  return <Modal title={`Upload-only links for ${node.name || "/"}`} onClose={close} className="operations-modal" labelledBy="upload-links-title">
    <p>Recipients can add uniquely named files to this folder. They cannot browse or download its contents. Anyone holding the link can upload within its limits.</p>
    <div className="operations-actions"><label>Days <input aria-label="Upload link days" type="number" min="1" max="30" value={days} onChange={(e) => setDays(Number(e.target.value))} /></label><label>Files <input aria-label="Upload link file limit" type="number" min="1" max="10000" value={files} onChange={(e) => setFiles(Number(e.target.value))} /></label><label>Total MiB <input aria-label="Upload link MiB limit" type="number" min="1" max="1048576" value={mib} onChange={(e) => setMib(Number(e.target.value))} /></label></div>
    <button className="btn primary" disabled={busy} onClick={() => void perform(async () => { const link = await api.createUploadLink(node.id, days, files, mib * 1024 * 1024); setUrl(`${location.origin}${link.url_path}`); })}>Create upload-only link</button>
    {url && <label>Copy this link now<input aria-label="New upload-only link" readOnly value={url} onFocus={(e) => e.target.select()} /></label>}
    {error && <p role="alert">{error}</p>}<ul className="operations-list">{links.map((link) => <li key={link.id}><span>Expires {new Date(link.expires_at).toLocaleString()} · {link.files_uploaded}/{link.max_files} files · {link.bytes_uploaded.toLocaleString()}/{link.max_bytes.toLocaleString()} bytes</span><button className="btn danger" disabled={busy} onClick={() => void perform(() => api.revokeUploadLink(node.id, link.id))}>Revoke upload link</button></li>)}</ul>
    <div className="modal-actions"><button className="btn" disabled={busy} onClick={close}>Done</button></div>
  </Modal>;
}
