import { beforeEach, describe, expect, it } from "vitest";
import {
  attachmentCacheKey,
  clearAttachmentCache,
  getAttachmentDataUri,
  setAttachmentDataUri,
} from "./attachmentCache";

describe("attachmentCache", () => {
  beforeEach(() => clearAttachmentCache());

  it("combines session and path into an isolated key", () => {
    expect(attachmentCacheKey("/tmp/chart.png", "session-1")).toBe(
      "session-1::/tmp/chart.png",
    );
    expect(attachmentCacheKey("/tmp/chart.png", null)).toBe("::/tmp/chart.png");
  });

  it("stores and reads data URIs by session and path", () => {
    setAttachmentDataUri(
      "/tmp/chart.png",
      "session-1",
      "data:image/png;base64,one",
    );

    expect(getAttachmentDataUri("/tmp/chart.png", "session-1")).toBe(
      "data:image/png;base64,one",
    );
    expect(getAttachmentDataUri("/tmp/chart.png", "session-2")).toBeNull();
  });
});
