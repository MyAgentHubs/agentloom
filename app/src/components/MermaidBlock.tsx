import { memo, useEffect, useId, useState } from "react";

let initialized = false;

export const MermaidBlock = memo(function MermaidBlock({
  code,
  complete,
}: {
  code: string;
  complete: boolean;
}) {
  const [svg, setSvg] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const diagramId = "mermaid-" + useId().replace(/[^a-zA-Z0-9-]/g, "");

  useEffect(() => {
    if (!complete) return;

    setSvg(null);
    setFailed(false);

    let cancelled = false;
    (async () => {
      try {
        const mermaid = (await import("mermaid")).default;
        if (!initialized) {
          mermaid.initialize({
            startOnLoad: false,
            securityLevel: "strict",
            theme: "neutral",
            suppressErrorRendering: true,
          });
          initialized = true;
        }
        const { svg } = await mermaid.render(diagramId, code);
        if (!cancelled) {
          setSvg(svg);
          setFailed(false);
        }
      } catch {
        document.getElementById(diagramId)?.remove();
        document.getElementById("d" + diagramId)?.remove();
        if (!cancelled) {
          setSvg(null);
          setFailed(true);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [code, complete]);

  if (!complete || failed || !svg) {
    return <pre className="mermaid-block__pre">{code}</pre>;
  }

  return (
    <div className="mermaid-block" dangerouslySetInnerHTML={{ __html: svg }} />
  );
});
