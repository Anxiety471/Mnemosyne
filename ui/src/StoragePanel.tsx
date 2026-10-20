import { useEffect, useState } from "react";
import { api, StorageStats } from "./api";
import { Modal } from "./Modal";

const bytes = (n: number) => `${(n / 1024 / 1024).toLocaleString(undefined, { maximumFractionDigits: 2 })} MiB`;

export function StoragePanel({ close }: { close: () => void }) {
  const [stats, setStats] = useState<StorageStats | null>(null);
  const [error, setError] = useState("");
  const refresh = async () => {
    try {
      setStats(await api.storage());
    } catch (e) {
      setError(String(e));
    }
  };
  useEffect(() => { void refresh(); }, []);
  return <Modal title="Storage" onClose={close} className="operations-modal" labelledBy="storage-title">
    {error && <p role="alert">{error}</p>}
    {stats && <><p>{stats.live_files.toLocaleString()} live files · {stats.revisions.toLocaleString()} revisions</p>
      {stats.quota_bytes !== null && <p>Workspace quota: {bytes(stats.revision_bytes)} / {bytes(stats.quota_bytes)} used by all revisions.</p>}
      <progress aria-label="Filesystem space used" max={stats.filesystem_total_bytes} value={stats.filesystem_total_bytes - stats.filesystem_available_bytes} />
      <p>{bytes(stats.filesystem_available_bytes)} available on the host filesystem</p>
      <ul className="operations-list">{[
        ["Live file contents", stats.live_bytes], ["All revision contents (before deduplication)", stats.revision_bytes],
        ["Blobs on disk", stats.blob_bytes], ["Unreferenced blobs (snapshot estimate)", stats.unreferenced_blob_bytes],
        ["Staged transfers", stats.staging_bytes], ["Database, WAL and shared memory", stats.database_bytes],
      ].map(([label, value]) => <li key={label}><strong>{label}</strong><span>{bytes(Number(value))}</span></li>)}</ul>
      <p>Revision totals include trash and history. Identical content shares a blob. Filesystem space includes other applications; these figures describe a point in time.</p></>}
    <div className="modal-actions"><button className="btn" onClick={() => void refresh()}>Refresh</button><button className="btn" onClick={close}>Done</button></div>
  </Modal>;
}
