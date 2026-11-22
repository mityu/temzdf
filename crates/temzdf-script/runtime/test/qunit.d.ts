// What temzdf/test (index.ts) uses of QUnit 3 (the npm package has no
// declarations of its own).
declare module "qunit" {
  export interface QUnitAssert {
    timeout(ms: number): void;
    pushResult(result: { result: boolean; actual: unknown; expected: unknown; message?: string }): void;
  }
  export interface QUnitError {
    passed: boolean;
    actual: unknown;
    expected: unknown;
    message?: string;
    stack?: string;
  }
  export interface QUnitTestEnd {
    name: string;
    fullName: string[];
    status: "passed" | "failed" | "skipped" | "todo";
    errors: QUnitError[];
  }
  export interface QUnitRunEnd {
    testCounts: { passed: number; failed: number; skipped: number; todo: number; total: number };
  }
  export interface QUnitStatic {
    config: { autostart: boolean; testTimeout: number | undefined; reorder: boolean; seed: string | undefined };
    dump: { parse(value: unknown): string };
    on(event: "runStart", fn: () => void): void;
    on(event: "testEnd", fn: (test: QUnitTestEnd) => void): void;
    on(event: "runEnd", fn: (run: QUnitRunEnd) => void): void;
    module(name: string, scope?: () => void): void;
    test(name: string, fn: (assert: QUnitAssert) => Promise<void>): void;
    start(): void;
  }
  const QUnit: QUnitStatic;
  export default QUnit;
}
