/* @fontsource 字体包入口是 CSS 文件，TS 无法解析其类型；这里声明为副作用模块 */
declare module "@fontsource-variable/inter";
declare module "@fontsource-variable/noto-sans-sc";
declare module "@fontsource-variable/jetbrains-mono";

declare module "*.png" {
  const src: string;
  export default src;
}
declare module "*.svg" {
  const src: string;
  export default src;
}
