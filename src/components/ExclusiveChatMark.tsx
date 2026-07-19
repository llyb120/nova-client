import { Show } from "solid-js";
import { EngravedNumberMark } from "./EngravedNumberMark";

/** token 前缀对应的会话背景编号，按声明顺序匹配。 */
const EXCLUSIVE_NUMBER_BY_TOKEN_PREFIX: ReadonlyArray<readonly [string, string]> = [
  ["you.bin", "000"],
  ["bin.ge", "000"],
  ["yao.mengjia", "001"],
  ["chen.lv", "002"],
  ["zheng.hanliang", "003"],
];

export function exclusiveNumberForToken(token: string): string | undefined {
  const normalized = token.trim();
  return EXCLUSIVE_NUMBER_BY_TOKEN_PREFIX.find(([prefix]) =>
    normalized.startsWith(prefix),
  )?.[1];
}

export function ExclusiveChatMark(props: { token: string }) {
  const number = () => exclusiveNumberForToken(props.token);

  return (
    <Show when={number()}>
      {(value) => (
        <div class="chat-engraved-watermark" aria-hidden="true">
          <EngravedNumberMark number={value()} class="chat-engraved-mark" />
        </div>
      )}
    </Show>
  );
}
