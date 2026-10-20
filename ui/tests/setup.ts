// React Testing Library only registers its automatic cleanup when `afterEach` is a global, and
// this suite imports vitest explicitly instead. Unmounting between tests keeps a previous
// render's DOM out of the next test's queries.
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(cleanup);
