// Element construction. Every visible string in this app comes from disk (paths,
// pack names, bootstrap output), so the whole UI is built through textContent and
// setAttribute rather than innerHTML - the one exception is the icon module,
// which inlines SVG shipped inside node_modules.

// `boolean` is in the union so `cond && el(...)` reads at the call site; `append`
// skips it rather than rendering "false".
export type Child = Node | string | number | boolean | null | undefined | Child[];

/** `class` sets className, `text` sets textContent, `on*` adds a listener.
 *  `false`/`null`/`undefined` removes an attribute instead of writing "false",
 *  which is what makes `disabled: someFlag` read correctly at the call site. */
export type Attrs = Record<string, unknown>;

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs?: Attrs | null,
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (attrs) {
    for (const [key, value] of Object.entries(attrs)) {
      if (value === false || value === null || value === undefined) continue;
      if (key === "class") node.className = String(value);
      else if (key === "text") node.textContent = String(value);
      else if (key.startsWith("on") && typeof value === "function") {
        node.addEventListener(key.slice(2).toLowerCase(), value as EventListener);
      } else if (value === true) node.setAttribute(key, "");
      else node.setAttribute(key, String(value));
    }
  }
  append(node, children);
  return node;
}

export function append(parent: Node, children: Child[]): void {
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    if (Array.isArray(child)) append(parent, child);
    else if (child instanceof Node) parent.appendChild(child);
    else parent.appendChild(document.createTextNode(String(child)));
  }
}

export function clear(node: Node): void {
  while (node.firstChild) node.removeChild(node.firstChild);
}

/** Replace a node's children in one shot. Used by every re-render in the app:
 *  the screens are small enough that diffing would cost more code than it saves,
 *  and a full subtree swap never leaves a stale row behind. */
export function fill(node: Element, ...children: Child[]): void {
  clear(node);
  append(node, children);
}
