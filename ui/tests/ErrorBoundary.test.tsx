import { render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { ErrorBoundary } from "../src/ErrorBoundary";

function Broken(): never {
  throw new Error("parser exploded");
}

afterEach(() => vi.restoreAllMocks());

test("shows the fallback when a child throws", () => {
  // React logs the caught error; the fallback is what the test is about.
  vi.spyOn(console, "error").mockImplementation(() => {});
  render(
    <ErrorBoundary message="This folder can't be listed">
      <Broken />
    </ErrorBoundary>,
  );

  expect(screen.getByRole("heading", { name: "This folder can't be listed" })).toBeTruthy();
  expect(screen.getByText("parser exploded")).toBeTruthy();
});

test("renders children when nothing throws", () => {
  render(
    <ErrorBoundary>
      <p>all good</p>
    </ErrorBoundary>,
  );

  expect(screen.getByText("all good")).toBeTruthy();
});
