import { useEffect, useRef, useState } from "react";
import { api, TransferRecord } from "./api";
import { Modal } from "./Modal";

export function TransfersPanel({ close }: { close: () => void }) {
  const [records, setRecords] = useState<TransferRecord[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const controller = useRef<AbortController | null>(null);
  const refresh = async () => { setRecords(await api.transfers()); };
  useEffect(() => {
    let current = true;
    const tick = () => api.transfers().then((items) => { if (current) setRecords(items); }).catch((e) => { if (current) setError(String(e)); });
    void tick();
    const timer = setInterval(tick, 2000);
    return () => { current = false; clearInterval(timer); controller.current?.abort(); };
  }, []);
  const perform = async (action: () => Promise<void>) => {
    setError("");
    try { await action(); await refresh(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  };
  const resume = (record: TransferRecord, file: File) => perform(async () => {
    const ctrl = new AbortController(); controller.current = ctrl; setBusy(record.id);
    try { await api.upload(record.path, file, () => {}, ctrl.signal, record.id); }
    catch (e) { if (!ctrl.signal.aborted) throw e; }
    finally { controller.current = null; setBusy(null); }
  });
  return <Modal title="Transfers" onClose={close} className="operations-modal" labelledBy="transfers-title">
    <p>Uploads survive restarts. Select the original file to resume. Downloads are handed to your browser; "issued" means the stream started.</p>
    {error && <p role="alert">{error}</p>}
    {!records.length && <p>No transfers yet.</p>}
    <ul className="operations-list">{records.map((record) => <li key={record.id}>
      <strong>{record.path}</strong><span>{record.direction} · {record.status} · {record.received.toLocaleString()} / {record.size.toLocaleString()} bytes</span>
      {record.direction === "upload" && <progress max={record.size || 1} value={record.received} aria-label={`Progress for ${record.path}`} />}
      {record.error && <span role="alert">{record.error}</span>}
      {record.direction === "upload" && !["completed", "cancelled", "expired"].includes(record.status) && <div className="operations-actions">
        {busy === record.id ? <button className="btn" onClick={() => controller.current?.abort()}>Pause</button> : <>
          {record.status === "active" && <button className="btn" disabled={busy !== null} onClick={() => perform(() => api.changeTransfer(record.id, "paused"))}>Pause</button>}
          <label className="btn">Resume / retry<input type="file" hidden disabled={busy !== null} onChange={(e) => { const file = e.target.files?.[0]; e.target.value = ""; if (file) void resume(record, file); }} /></label>
          <button className="btn danger" disabled={busy !== null} onClick={() => perform(() => api.cancelTransfer(record.id))}>Cancel transfer</button>
        </>}
      </div>}
    </li>)}</ul>
    <div className="modal-actions"><button className="btn" onClick={close}>Done</button></div>
  </Modal>;
}
