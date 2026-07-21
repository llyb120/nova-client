import { Show } from "solid-js";
import { EngravedNumberMark } from "./EngravedNumberMark";
import { signatureProgress, signatureVisible } from "./signatureOverlay";

/** token 前缀对应的会话背景身份，按声明顺序匹配。 */
const EXCLUSIVE_MARK_BY_TOKEN_PREFIX: ReadonlyArray<readonly [string, string]> = [
  ["you.bin", "000"],
  ["bin.ge", "000"],
  ["yao.mengjia", "001"],
  ["chen.lv", "002"],
  ["zheng.hanliang", "003"],
];

export interface ExclusiveChatIdentity {
  username: string;
  number: string;
}

export function exclusiveIdentityForToken(token: string): ExclusiveChatIdentity | undefined {
  const normalized = token.trim();
  const match = EXCLUSIVE_MARK_BY_TOKEN_PREFIX.find(([prefix]) =>
    normalized.startsWith(prefix),
  );
  return match ? { username: match[0], number: match[1] } : undefined;
}

export function exclusiveNumberForToken(token: string): string | undefined {
  return exclusiveIdentityForToken(token)?.number;
}

export function ExclusiveChatMark(props: { token: string }) {
  const identity = () => exclusiveIdentityForToken(props.token);
  /** 启动签名进度 → 斜边软刷遮罩：前沿带柔边，水印像被笔尖扫过一样从左到右显出。 */
  const revealMask = () => {
    const p = signatureProgress();
    if (p === null) return undefined;
    const edge = p * 118;
    return `linear-gradient(100deg, #000 ${(edge - 18).toFixed(1)}%, transparent ${edge.toFixed(1)}%)`;
  };

  return (
    <Show when={identity()}>
      {(value) => (
        <div
          class="composer-engraved-watermark"
          classList={{
            "awaiting-signature": !signatureVisible(),
            signing: signatureProgress() !== null,
          }}
          aria-hidden="true"
          style={
            revealMask()
              ? {
                  "mask-image": revealMask()!,
                  "-webkit-mask-image": revealMask()!,
                  // 默认按边框盒裁剪会切掉花体溢出笔画；no-clip 保留完整字迹。
                  "mask-clip": "no-clip",
                  "-webkit-mask-clip": "no-clip",
                }
              : undefined
          }
        >
          <EngravedNumberMark
            username={value().username}
            number={value().number}
            class="composer-engraved-mark"
          />
        </div>
      )}
    </Show>
  );
}
