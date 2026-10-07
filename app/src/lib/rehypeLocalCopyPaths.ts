import type { Root, RootContent } from "hast";
import type { Plugin } from "unified";
import { copyPathFromUrl } from "./chatFilePath";

declare module "hast" {
  interface ElementData {
    localCopyPath?: string;
  }
}

function preserveCopyPath(node: Root | RootContent): void {
  if (node.type === "element") {
    const url =
      node.tagName === "a"
        ? node.properties.href
        : node.tagName === "img"
          ? node.properties.src
          : undefined;
    if (typeof url === "string") {
      const path = copyPathFromUrl(url);
      if (path !== undefined) node.data = { ...node.data, localCopyPath: path };
    }
  }
  if ("children" in node) node.children.forEach(preserveCopyPath);
}

// HAST metadata survives react-markdown's URL sanitization but is not a DOM
// attribute or navigation URL. Only the copy action consumes this value.
export const rehypeLocalCopyPaths: Plugin<[], Root> = () => preserveCopyPath;
