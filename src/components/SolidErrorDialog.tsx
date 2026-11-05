import { onSettled } from "solid-js";

export function SolidErrorDialog(props: { error: () => unknown }) {
  let dialog!: HTMLDialogElement;
  const error = props.error();
  const detail = error instanceof Error ? error.stack ?? error.message : String(error);
  onSettled(() => dialog.showModal());

  return (
    <dialog ref={dialog} class="modal" role="alertdialog" aria-modal="true" aria-labelledby="solid-error-title"
      style={{ padding: "0", color: "var(--text)" }} onCancel={event => event.preventDefault()}>
      <div class="modal-head" id="solid-error-title">SolidJS 界面错误</div>
      <div class="modal-body">
        <pre style={{ margin: "0", "white-space": "pre-wrap", "overflow-wrap": "anywhere", "user-select": "text" }}>{detail}</pre>
        <button type="button" class="btn primary" autofocus onClick={() => location.reload()}>重新加载窗口</button>
      </div>
    </dialog>
  );
}
