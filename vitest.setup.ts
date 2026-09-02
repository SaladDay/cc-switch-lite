import "@testing-library/jest-dom/vitest";
import React from "react";
import { vi } from "vitest";

vi.mock("framer-motion", () => {
  const MotionDiv = React.forwardRef<
    HTMLDivElement,
    React.HTMLAttributes<HTMLDivElement> & {
      initial?: unknown;
      animate?: unknown;
      exit?: unknown;
      transition?: unknown;
    }
  >(({ initial, animate, exit, transition, ...props }, ref) => {
    void initial;
    void animate;
    void exit;
    void transition;
    return React.createElement("div", { ...props, ref });
  });

  MotionDiv.displayName = "MotionDiv";
  return {
    AnimatePresence: ({ children }: React.PropsWithChildren) => children,
    motion: { div: MotionDiv },
    useIsPresent: () => true,
    useReducedMotion: () => true,
  };
});

const storedValues = new Map<string, string>();
const localStorageMock: Storage = {
  get length() {
    return storedValues.size;
  },
  clear: () => storedValues.clear(),
  getItem: (key) => storedValues.get(key) ?? null,
  key: (index) => Array.from(storedValues.keys())[index] ?? null,
  removeItem: (key) => storedValues.delete(key),
  setItem: (key, value) => storedValues.set(key, String(value)),
};

Object.defineProperty(window, "localStorage", {
  configurable: true,
  value: localStorageMock,
});
