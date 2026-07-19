import { createMemo } from "solid-js";
import "./EngravedNumberMark.css";

interface EngravedNumberMarkProps {
  /** 编号中的数字部分；会自动过滤空格等非数字字符。 */
  number: string | number;
  /** 与编号一起刻出的用户名；仅保留英文字母和点。 */
  username?: string;
  class?: string;
  title?: string;
}

/** 可嵌在姓名、列表或按钮中的花体金属凹刻编号。 */
export function EngravedNumberMark(props: EngravedNumberMarkProps) {
  const digits = createMemo(() => String(props.number).replace(/\D/g, ""));
  const username = createMemo(() => (props.username ?? "").replace(/[^A-Za-z.]/g, ""));
  const label = createMemo(() =>
    username() ? `No. ${digits()} ${username()}` : `No. ${digits()}`,
  );

  return (
    <span
      class={`engraved-number-mark${props.class ? ` ${props.class}` : ""}`}
      aria-label={label()}
      title={props.title ?? label()}
    >
      <span class="engraved-number-mark-text" aria-hidden="true">
        <span class="engraved-number-mark-serial">No. {digits()}</span>
        {username() && <span class="engraved-number-mark-name">{username()}</span>}
      </span>
    </span>
  );
}
