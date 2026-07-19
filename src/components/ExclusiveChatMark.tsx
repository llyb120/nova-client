import { Show } from "solid-js";
import { EngravedNumberMark } from "./EngravedNumberMark";

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

  return (
    <Show when={identity()}>
      {(value) => (
        <div class="composer-engraved-watermark" aria-hidden="true">
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
