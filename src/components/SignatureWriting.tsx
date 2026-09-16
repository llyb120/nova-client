import { createMemo, createSignal, createUniqueId, For, onMount } from "solid-js";
import { signatureFrame, signaturePaths } from "./signaturePaths";
import { signatureProgress } from "./signatureOverlay";

export function SignatureWriting(props: { username: string }) {
  const drawing = signaturePaths[props.username.toLowerCase()];
  const gradient = createUniqueId();
  const paths: SVGPathElement[] = [];
  const [lengths, setLengths] = createSignal<number[]>([]);
  onMount(() => setLengths(paths.map((path) => path.getTotalLength())));
  const frame = createMemo(() => signatureFrame(lengths(), signatureProgress() ?? 1));

  return (
    <svg class="signature-writing" viewBox={`0 0 ${drawing.width} 94`}
      style={{ width: `${drawing.width / 65}em` }} aria-hidden="true">
      <defs>
        <linearGradient id={gradient} x1="0" y1="0" x2="1" y2="0">
          <stop offset="0" stop-color="var(--signature-from)" />
          <stop offset="0.48" stop-color="var(--signature-mid)" />
          <stop offset="1" stop-color="var(--signature-to)" />
        </linearGradient>
      </defs>
      <g fill="none" stroke={`url(#${gradient})`} stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
        <For each={drawing.strokes}>{(d, index) => (
          <path ref={(el) => { paths[index()] = el; }} d={d} pathLength="1"
            stroke-dasharray="1 1"
            stroke-dashoffset={lengths()[index()] ? 1 - frame()[index()].drawn / lengths()[index()] : 1}
            visibility={frame()[index()]?.drawn > 0 ? "visible" : "hidden"} />
        )}</For>
      </g>
    </svg>
  );
}
