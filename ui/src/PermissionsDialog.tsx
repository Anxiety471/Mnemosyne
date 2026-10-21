// Who else can reach a file or folder. Nodes without grants stay open to every actor.
import { useEffect, useState } from "react";
import { api, Node, Permission } from "./api";

const ROLES: Permission["role"][] = ["reader", "writer", "owner"];

export function PermissionsDialog({
  node,
  close,
  notify,
}: {
  node: Node;
  close: () => void;
  notify: (message: string, tone?: "ok" | "error") => void;
}) {
  const [grants, setGrants] = useState<Permission[]>([]);
  const [principal, setPrincipal] = useState("");
  const [role, setRole] = useState<Permission["role"]>("reader");
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let current = true;
    api.permissions(node.id)
      .then((found) => { if (current) setGrants(found); })
      .catch((e) => { if (current) notify(e instanceof Error ? e.message : String(e), "error"); })
      .finally(() => { if (current) setLoading(false); });
    return () => { current = false; };
  }, [node.id, notify]);

  const perform = async (action: () => Promise<Permission[]>, done: string) => {
    setBusy(true);
    try {
      setGrants(await action());
      notify(done);
    } catch (e) {
      notify(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const add = () => {
    const name = principal.trim();
    if (!name) return;
    void perform(() => api.setPermission(node.id, name, role), `Granted ${name} as ${role}`);
    setPrincipal("");
  };

  return (
    <div className="modal-scrim" onMouseDown={busy ? undefined : close}>
      <section
        className="modal share-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="permissions-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <h3 id="permissions-title">Sharing for “{node.name}”</h3>
        <p className="muted small">
          A node with no grants is open to every account on this Drawer. Adding a principal limits it to the roles below.
          Owners can change grants, writers can change content, readers can only read.
        </p>
        <label>
          Principal (username or account id)
          <input
            aria-label="Principal"
            value={principal}
            autoComplete="off"
            onChange={(e) => setPrincipal(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && add()}
          />
        </label>
        <label>
          Role
          <select aria-label="Role" value={role} onChange={(e) => setRole(e.target.value as Permission["role"])}>
            {ROLES.map((r) => (
              <option key={r} value={r}>{r}</option>
            ))}
          </select>
        </label>
        <div className="modal-actions">
          <button type="button" className="btn primary" disabled={busy || !principal.trim()} onClick={add}>
            Add grant
          </button>
        </div>
        <h4>Current grants</h4>
        {loading ? (
          <p className="muted small">Loading grants…</p>
        ) : grants.length === 0 ? (
          <p className="muted small">No grants. Everyone with access to this Drawer can use this {node.kind}.</p>
        ) : (
          <ul className="share-links">
            {grants.map((grant) => (
              <li key={grant.principal}>
                <span className="muted small">
                  {grant.principal} · {grant.role}
                </span>
                <button
                  type="button"
                  className="btn danger"
                  disabled={busy}
                  onClick={() => void perform(() => api.removePermission(node.id, grant.principal), `Removed ${grant.principal}`)}
                >
                  Remove
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="modal-actions">
          <button type="button" className="btn" onClick={close}>Done</button>
        </div>
      </section>
    </div>
  );
}
