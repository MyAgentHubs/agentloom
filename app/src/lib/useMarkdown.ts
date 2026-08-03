import { useEffect, useState } from "react";
import type { MarkdownBody as MarkdownBodyType } from "../components/MarkdownBody";
import type * as MarkdownLib from "./markdownLib";

let cachedMarkdown: typeof MarkdownBodyType | null = null;
let cachedMarkdownLib: typeof MarkdownLib | null = null;

function loadMarkdownBody(): Promise<typeof MarkdownBodyType> {
  return import("../components/MarkdownBody").then((module) => {
    cachedMarkdown = module.MarkdownBody;
    return cachedMarkdown;
  });
}

function loadMarkdownLib(): Promise<typeof MarkdownLib> {
  return import("./markdownLib").then((module) => {
    cachedMarkdownLib = module;
    return cachedMarkdownLib;
  });
}

export function preloadMarkdown(): Promise<typeof MarkdownBodyType> {
  return Promise.all([loadMarkdownBody(), loadMarkdownLib()]).then(
    ([MarkdownBody]) => MarkdownBody,
  );
}

export function useMarkdown(): typeof MarkdownBodyType | null {
  const [Component, setComponent] = useState<typeof MarkdownBodyType | null>(
    () => cachedMarkdown,
  );
  useEffect(() => {
    if (!cachedMarkdown) {
      void loadMarkdownBody().then((MarkdownBody) => {
        setComponent(() => MarkdownBody);
      });
    }
  }, []);
  return Component;
}

export function useMarkdownLib(): typeof MarkdownLib | null {
  const [lib, setLib] = useState<typeof MarkdownLib | null>(
    () => cachedMarkdownLib,
  );
  useEffect(() => {
    if (!cachedMarkdownLib) {
      void loadMarkdownLib().then(setLib);
    }
  }, []);
  return lib;
}
