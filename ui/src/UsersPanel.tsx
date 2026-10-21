import { useEffect, useState } from "react";
import { api, UserAccount } from "./api";
import { Modal } from "./Modal";

export function UsersPanel({ close }: { close: () => void }) {
  const [users, setUsers] = useState<UserAccount[]>([]);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [mib, setMib] = useState(1024);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [quotas, setQuotas] = useState<Record<string, number>>({});
  const [confirmDisable, setConfirmDisable] = useState<UserAccount | null>(null);
  useEffect(() => { let active = true; api.users().then((items) => { if (active) setUsers(items); }).catch((e) => { if (active) setError(String(e)); }); return () => { active = false; }; }, []);
  const perform = async (action: () => Promise<void>) => { setBusy(true); setError(""); try { await action(); setUsers(await api.users()); } catch (e) { setError(String(e)); } finally { setBusy(false); } };
  const doDisable = async () => {
    if (!confirmDisable) return;
    setBusy(true); setError("");
    try { await api.changeUser(confirmDisable.id, { disabled: !confirmDisable.disabled }); setUsers(await api.users()); setConfirmDisable(null); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  return <>
    <Modal title="Users and quotas" onClose={close} className="operations-modal" labelledBy="users-title">
      <p>Each user has a private workspace. Quotas count all revision bytes, including history and trash. Lowering a quota below current usage blocks new writes. Share files through download or upload-only links.</p>
      <form onSubmit={(e) => { e.preventDefault(); void perform(async () => { await api.createUser(username, password, Math.round(mib * 1024 * 1024)); setUsername(""); setPassword(""); }); }}>
        <label>Username <input autoComplete="off" pattern="[a-z0-9_-]{1,32}" required value={username} onChange={(e) => setUsername(e.target.value)} /></label>
        <label>Password <input type="password" autoComplete="new-password" minLength={12} required value={password} onChange={(e) => setPassword(e.target.value)} /></label>
        <label>Quota MiB <input type="number" min="1" required value={mib} onChange={(e) => setMib(Number(e.target.value))} /></label><button className="btn primary" disabled={busy}>Create user</button>
      </form>{error && <p role="alert">{error}</p>}
      <ul className="operations-list">{users.map((user) => {
        // The input holds a pending edit; until it is touched the user's stored quota shows.
        const edited = quotas[user.id];
        const quota = edited ?? user.quota_bytes / 1024 / 1024;
        return <li key={user.id}><strong>{user.username} · {user.disabled ? "Disabled" : "Active"}</strong><div className="operations-actions"><label>Quota MiB <input type="number" min="1" value={quota} onChange={(e) => setQuotas((q) => ({ ...q, [user.id]: Number(e.target.value) }))} /></label><button className="btn" disabled={busy || !edited || edited < 1} onClick={() => void perform(() => api.changeUser(user.id, { quota_bytes: Math.round(quota * 1024 * 1024) }))}>Save quota</button><button className="btn" disabled={busy} onClick={() => setConfirmDisable(user)}>{user.disabled ? "Enable" : "Disable"}</button></div></li>;
      })}</ul>
      <div className="modal-actions"><button className="btn" onClick={close}>Done</button></div>
    </Modal>
    {confirmDisable && <Modal title={`${confirmDisable.disabled ? "Enable" : "Disable"} ${confirmDisable.username}?`} onClose={() => setConfirmDisable(null)} labelledBy="confirm-disable-title">
      {confirmDisable.disabled
        ? <p>{confirmDisable.username} will be able to sign in and access their workspace again.</p>
        : <p>{confirmDisable.username} will be signed out immediately and will be unable to sign in. Their files remain intact and can be re-enabled later.</p>}
      <div className="modal-actions"><button className="btn" onClick={() => setConfirmDisable(null)}>Cancel</button><button className="btn danger" disabled={busy} onClick={() => void doDisable()}>{confirmDisable.disabled ? "Enable user" : "Disable user"}</button></div>
    </Modal>}
  </>;
}
