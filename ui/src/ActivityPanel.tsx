import { useEffect, useState } from "react";
import { api, ActivityEvent } from "./api";
import { Modal } from "./Modal";

export function ActivityPanel({ close }: { close: () => void }) {
  const [events, setEvents] = useState<ActivityEvent[]>([]);
  const [error, setError] = useState("");
  const [more, setMore] = useState(true);
  const [busy, setBusy] = useState(false);
  const [initialLoading, setInitialLoading] = useState(true);
  useEffect(() => { let active = true; api.activity().then((items) => { if (active) { setEvents(items); setMore(items.length === 100); } }).catch((e) => { if (active) setError(String(e)); }).finally(() => { if (active) setInitialLoading(false); }); return () => { active = false; }; }, []);
  const older = async () => { setBusy(true); try { const items = await api.activity(events.at(-1)?.id); setEvents((old) => [...old, ...items]); setMore(items.length === 100); } catch (e) { setError(String(e)); } finally { setBusy(false); } };
  return <Modal title="Activity history" onClose={close} className="operations-modal" labelledBy="activity-title">
    <p>File reads, archive requests, changes, sharing, logins and MCP requests. Status indicates the HTTP response; downloads may still be in progress. Route templates hide public link secrets.</p>
    {error && <p role="alert">{error}</p>}
    {initialLoading && <p role="status">Loading…</p>}
    {!initialLoading && <ul className="operations-list">{events.map((event) => <li key={event.id}><strong>{event.method} {event.route} · {event.status}</strong><span>{event.actor} · {new Date(event.created_at).toLocaleString()}</span></li>)}</ul>}
    {!initialLoading && !events.length && <p>No activity recorded yet.</p>}
    <div className="modal-actions">{more && <button className="btn" disabled={busy} onClick={() => void older()}>{busy ? "Loading…" : "Load older events"}</button>}<button className="btn" onClick={close}>Done</button></div>
  </Modal>;
}
