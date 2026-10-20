// The modal's inert scoping is a behaviour the browser depends on: if it inerts its own subtree
// (as it did when the scrim was assumed to be a child of #root), every click and keystroke in the
// dialog is swallowed. These tests pin the scope and the focus handoff.
import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { Modal } from "../src/Modal";

/** Mirrors the real tree: #root holds `.shell`, which holds the app chrome and any open modal. */
function Shell({ children }: { children: React.ReactNode }) {
  return (
    <div className="shell">
      <main>the app behind the modal</main>
      {children}
    </div>
  );
}

function renderShell(ui: React.ReactNode) {
  const root = document.createElement("div");
  root.id = "root";
  document.body.append(root);
  return render(<Shell>{ui}</Shell>, { container: root });
}

test("inerts the app behind the dialog but leaves the dialog itself interactive", () => {
  renderShell(
    <Modal title="Access token" onClose={() => {}}>
      <input aria-label="Token" />
    </Modal>,
  );

  expect(screen.getByText("the app behind the modal").closest("main")?.hasAttribute("inert")).toBe(true);
  expect(screen.getByRole("dialog").closest("[inert]")).toBeNull();
  expect(screen.getByLabelText("Token")).toBeTruthy();
});

test("focuses the initial field and restores focus to the trigger on close", () => {
  const trigger = document.createElement("button");
  trigger.textContent = "open";
  document.body.append(trigger);
  trigger.focus();

  const { unmount } = renderShell(
    <Modal title="Rename" onClose={() => {}}>
      <input aria-label="New name" />
    </Modal>,
  );
  expect(document.activeElement).toBe(screen.getByLabelText("New name"));

  unmount();
  expect(document.activeElement).toBe(trigger);
  expect(document.querySelector("[inert]")).toBeNull();
});
