export function mountRoot(): HTMLElement {
  const root = document.createElement("div");
  document.body.replaceChildren(root);
  return root;
}
