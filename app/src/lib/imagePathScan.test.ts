import { describe, expect, it } from "vitest";
import { scanImagePaths } from "./imagePathScan";

describe("scanImagePaths", () => {
  it("抽取反引号内的绝对图片路径", () => {
    expect(
      scanImagePaths("文件已保存： `/Users/alice/local/sid/sea_moon.jpg` 完成"),
    ).toEqual(["/Users/alice/local/sid/sea_moon.jpg"]);
  });

  it("抽取句中混文字的裸绝对路径", () => {
    expect(scanImagePaths("结果在 /a/b/chart.png 里，请查收")).toEqual([
      "/a/b/chart.png",
    ]);
  });

  it("抽取 file:// scheme 并剥掉 scheme", () => {
    expect(scanImagePaths("已保存 file:///abs/x.svg 供预览")).toEqual([
      "/abs/x.svg",
    ]);
  });

  it("抽取 <...> 包裹且内部含空格的路径", () => {
    expect(
      scanImagePaths("详见 </Users/alice/my pics/a b.png> 这张图"),
    ).toEqual(["/Users/alice/my pics/a b.png"]);
  });

  it("抽取 ~ 开头的路径", () => {
    expect(scanImagePaths("在 ~/Pictures/cat.webp 里")).toEqual([
      "~/Pictures/cat.webp",
    ]);
  });

  it("抽取独立成行的路径", () => {
    expect(scanImagePaths("/abs/only/line.gif")).toEqual([
      "/abs/only/line.gif",
    ]);
  });

  it("同一路径出现两次只返回一次", () => {
    expect(
      scanImagePaths("先看 `/a/dup.png`，然后再看一次 /a/dup.png 确认"),
    ).toEqual(["/a/dup.png"]);
  });

  it("已是 ![]() 语法的路径不重复抽取", () => {
    expect(scanImagePaths("![chart](/a/already.png) 见上图")).toEqual([]);
  });

  it("非图片后缀（.json/.txt/.md）不抽取", () => {
    expect(scanImagePaths("配置在 /a/b/config.json 里")).toEqual([]);
    expect(scanImagePaths("日志在 /a/b/out.txt 里")).toEqual([]);
    expect(scanImagePaths("文档在 /a/b/readme.md 里")).toEqual([]);
  });

  it("相对路径不抽取", () => {
    expect(scanImagePaths("图在 assets/x.png 里")).toEqual([]);
    expect(scanImagePaths("图在 ./a.png 里")).toEqual([]);
  });

  it("协议相对路径（//host/a.png）不抽取", () => {
    expect(scanImagePaths("见 //host/a.png")).toEqual([]);
  });

  it("host 非空的 file:// URL 不抽取", () => {
    expect(scanImagePaths("见 file://host/a.png")).toEqual([]);
  });

  it("Windows 盘符路径可识别", () => {
    expect(scanImagePaths(String.raw`详见 C:\tmp\x.png 文件`)).toEqual([
      String.raw`C:\tmp\x.png`,
    ]);
  });

  it("去掉尾随全角标点后仍可识别（反引号内）", () => {
    expect(scanImagePaths("请看 `/a/b.png，`还有更多")).toEqual(["/a/b.png"]);
  });

  it("带查询串的裸路径不抽取（避免把 URL 误判成本地路径）", () => {
    expect(scanImagePaths("见 /a/b.png?x=1")).toEqual([]);
  });

  it("空字符串返回空数组", () => {
    expect(scanImagePaths("")).toEqual([]);
  });
});
