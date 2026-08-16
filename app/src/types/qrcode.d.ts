declare module "qrcode" {
  type SvgOptions = {
    type: "svg";
    errorCorrectionLevel?: string;
    margin?: number;
    // 真机踩坑：不传 width 时生成的 svg 只带 viewBox、无显式 width/height 属性，在
    // WKWebView 的 flex 容器里会塌成 0×0——传了库才会把 width/height 写进 svg 标签。
    width?: number;
  };

  const QRCode: {
    toString(text: string, options: SvgOptions): Promise<string>;
  };

  export default QRCode;
}
