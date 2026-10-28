// Regressify image simplification page library.
//
// Rust evaluates this file in the page's main document as the body of
//   (() => { 'use strict'; const options = <JSON>; const operation = <JSON>; <this file> })()
// once per operation, with no observers, timers, or state kept between calls.
// Every value comes from the typed `options` object; no configuration text is
// pasted into JavaScript or CSS source.
//
// Operations:
// - ensure:  install the hidden SVG filter host and a constructed stylesheet.
// - mark:    flag qualifying images so the stylesheet pixelates them; return counts.
// - refresh: re-run only the paint-order guard for flagged images, for selector
//            captures that scrolled after marking.
// - clear:   remove every flag so later steps see the unmodified page.

const SVG_NS = 'http://www.w3.org/2000/svg';
const ATTRIBUTE = options.attribute;
// Flag values: PIXELATE images match the filter rule; SHARP images qualified but
// the paint-order guard kept them unfiltered. Only PIXELATE changes rendering.
const PIXELATE = options.pixelate;
const SHARP = options.sharp;
// Sub-pixel tolerance so boxes that merely share an edge do not count as overlap.
const OVERLAP = 0.5;
const EPSILON = 1e-6;
const UNBOUNDED = Object.freeze({ left: -Infinity, top: -Infinity, right: Infinity, bottom: Infinity });
const REPLACED = new Set(['IMG', 'VIDEO', 'CANVAS', 'IFRAME', 'EMBED', 'OBJECT', 'INPUT', 'TEXTAREA', 'SELECT', 'METER', 'PROGRESS', 'AUDIO']);

const svgNode = (tag, attributes, children = []) => {
  const element = document.createElementNS(SVG_NS, tag);
  for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, String(value));
  for (const child of children) element.appendChild(child);
  return element;
};

// Uniform block x block averages aligned to the element's border box:
// 1. `mean` holds, at every pixel, the average of the block that starts there
//    (edge pixels are duplicated for partial blocks at the right/bottom edge).
// 2. A tiled single-pixel grid keeps one mean per block, at the block's start.
// 3. Two separable copy passes (divisor 1, so values are copied exactly) spread
//    each sample over its block. Unlike centred samples plus dilation, partial
//    edge blocks and images smaller than a block are filled instead of left
//    transparent. Chromium caps convolution kernels at 16x16, hence block <= 16.
const createHost = (block) => {
  const ones = (count) => Array(count).fill('1').join(' ');
  const filter = svgNode('filter', {
    id: options.filterId, x: 0, y: 0, width: 1, height: 1,
    filterUnits: 'objectBoundingBox', primitiveUnits: 'userSpaceOnUse',
    'color-interpolation-filters': 'sRGB',
  }, [
    svgNode('feConvolveMatrix', {
      in: 'SourceGraphic', order: block, kernelMatrix: ones(block * block), divisor: block * block,
      targetX: 0, targetY: 0, edgeMode: 'duplicate', preserveAlpha: 'false', result: 'mean',
    }),
    svgNode('feFlood', { x: 0, y: 0, width: 1, height: 1, 'flood-color': '#000', 'flood-opacity': 1, result: 'dot' }),
    svgNode('feOffset', { in: 'dot', dx: 0, dy: 0, x: 0, y: 0, width: block, height: block, result: 'cell' }),
    svgNode('feTile', { in: 'cell', result: 'grid' }),
    svgNode('feComposite', { in: 'mean', in2: 'grid', operator: 'in', result: 'samples' }),
    svgNode('feConvolveMatrix', {
      in: 'samples', order: `${block} 1`, kernelMatrix: ones(block), divisor: 1,
      targetX: block - 1, targetY: 0, edgeMode: 'none', preserveAlpha: 'false', result: 'rows',
    }),
    svgNode('feConvolveMatrix', {
      in: 'rows', order: `1 ${block}`, kernelMatrix: ones(block), divisor: 1,
      targetX: 0, targetY: block - 1, edgeMode: 'none', preserveAlpha: 'false',
    }),
  ]);
  const host = svgNode('svg', {
    id: options.hostId, width: 0, height: 0, 'aria-hidden': 'true', focusable: 'false',
    'data-block-size': block,
  }, [svgNode('defs', {}, [filter])]);
  // CSSOM writes are allowed under a strict style-src CSP, unlike a style
  // attribute or <style> element. Out of flow and zero-sized so the host
  // cannot change layout, and outside <body> so body selectors ignore it.
  for (const [name, value] of [
    ['position', 'absolute'], ['left', '0'], ['top', '0'], ['width', '0'], ['height', '0'],
    ['overflow', 'hidden'], ['pointer-events', 'none'], ['display', 'block'],
  ]) host.style.setProperty(name, value, 'important');
  return host;
};

const ownSheet = (sheet) => {
  try {
    return sheet.cssRules.length === 1 && sheet.cssRules[0].cssText.includes(options.filterId);
  } catch (_) {
    return false;
  }
};

// Re-creates the host or stylesheet when a single-page app removed or replaced them.
const ensure = () => {
  const root = document.documentElement;
  if (!root) return false;
  const block = options.blockSize;
  let host = document.getElementById(options.hostId);
  const filter = document.getElementById(options.filterId);
  const current = host !== null && host.parentNode === root &&
    host.getAttribute('data-block-size') === String(block) &&
    filter !== null && filter.parentNode !== null && filter.parentNode.parentNode === host;
  if (!current) {
    if (host !== null) host.remove();
    host = createHost(block);
    root.appendChild(host);
  }
  // A constructed sheet applies under strict style-src CSP, where an injected
  // <style> element is silently ignored.
  if (!document.adoptedStyleSheets.some(ownSheet)) {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(`[${ATTRIBUTE}="${PIXELATE}"] { filter: url("#${options.filterId}") !important; }`);
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  }
  return true;
};

const clear = () => {
  const flagged = document.querySelectorAll(`[${ATTRIBUTE}]`);
  for (const element of flagged) element.removeAttribute(ATTRIBUTE);
  return flagged.length;
};

const near = (value, target) => Math.abs(value - target) < EPSILON;
const integral = (value) => near(value, Math.round(value));

// Pixel-exact only for unscaled, unskewed, axis-aligned rendering. Scaling,
// zoom, skew, or fractional translation resamples the filtered result into
// seams or a single colour, so such images stay sharp.
const axisAligned = (a, b, c, d) =>
  (near(b, 0) && near(c, 0) && near(Math.abs(a), 1) && near(Math.abs(d), 1)) ||
  (near(a, 0) && near(d, 0) && near(Math.abs(b), 1) && near(Math.abs(c), 1));

const alignedMatrix = (value) => {
  if (value === 'none') return true;
  const match = /^matrix(3d)?\(([^)]*)\)$/.exec(value);
  if (match === null) return false;
  const m = match[2].split(',').map(Number);
  if (m.some((number) => !Number.isFinite(number))) return false;
  if (match[1] !== undefined) {
    // Column-major: only 2D-equivalent matrices without depth or perspective qualify.
    if (m.length !== 16 || ![2, 3, 6, 7, 8, 9, 11, 14].every((index) => near(m[index], 0)) ||
      !near(m[10], 1) || !near(m[15], 1)) return false;
    return axisAligned(m[0], m[1], m[4], m[5]) && integral(m[12]) && integral(m[13]);
  }
  return m.length === 6 && axisAligned(m[0], m[1], m[2], m[3]) && integral(m[4]) && integral(m[5]);
};

const alignedTranslate = (value) => value === 'none' || value.split(/\s+/).every((part, index) => {
  const match = /^(-?[\d.]+(?:e[-+]?\d+)?)px$/.exec(part);
  return match !== null && integral(Number(match[1])) && (index < 2 || near(Number(match[1]), 0));
});

const alignedRotate = (value) => {
  if (value === 'none') return true;
  const match = /^(?:z |0 0 1 )?(-?[\d.]+(?:e[-+]?\d+)?)deg$/.exec(value);
  return match !== null && integral(Number(match[1]) / 90);
};

const alignedScale = (value) => value === 'none' ||
  value.split(/\s+/).every((part, index) => near(Math.abs(Number(part)), 1) && (index < 2 || near(Number(part), 1)));

const alignedTransform = (style) => alignedMatrix(style.transform) && alignedTranslate(style.translate) &&
  alignedRotate(style.rotate) && alignedScale(style.scale);

// Composed-tree parent: slotted nodes render inside their slot, shadow roots inside their host.
const flatParent = (node) => {
  if (node.assignedSlot) return node.assignedSlot;
  const parent = node.parentNode;
  if (parent instanceof ShadowRoot) return parent.host;
  return parent instanceof Element ? parent : null;
};

const pixelAligned = (element) => {
  if (element.ownerSVGElement) {
    // SVG user units may be scaled by a viewBox or transforms; fractional SVG
    // positions are fine because SVG geometry is not resampled after filtering.
    const matrix = element.getScreenCTM();
    if (matrix === null || !axisAligned(matrix.a, matrix.b, matrix.c, matrix.d)) return false;
  }
  for (let node = element; node !== null; node = flatParent(node)) {
    const style = getComputedStyle(node);
    const zoom = Number.parseFloat(style.zoom);
    if (Number.isFinite(zoom) && !near(zoom, 1)) return false;
    if (style.offsetPath && style.offsetPath !== 'none') return false;
    if (node.ownerSVGElement) continue;
    if (!alignedTransform(style)) return false;
  }
  return true;
};

const alphaOf = (color) => {
  if (!color || color === 'transparent') return 0;
  const legacy = /^rgba?\(([^)]*)\)$/.exec(color);
  if (legacy !== null) {
    const parts = legacy[1].split(/[\s,/]+/).filter(Boolean);
    return parts.length >= 4 ? Number.parseFloat(parts[3]) : 1;
  }
  const modern = /\/\s*([\d.]+)(%?)\s*\)$/.exec(color);
  if (modern !== null) return Number.parseFloat(modern[1]) / (modern[2] ? 100 : 1);
  return 1;
};

const SIDES = ['Top', 'Right', 'Bottom', 'Left'];
const hasOutline = (style) => style.outlineStyle !== 'none' && Number.parseFloat(style.outlineWidth) > 0;

// Whether an element paints anything of its own box (not its children).
const paintsBox = (style) => style.backgroundImage !== 'none' || alphaOf(style.backgroundColor) > 0 ||
  style.borderImageSource !== 'none' || style.boxShadow !== 'none' || hasOutline(style) ||
  (style.backdropFilter !== undefined && style.backdropFilter !== 'none') ||
  SIDES.some((side) => {
    const borderStyle = style[`border${side}Style`];
    return borderStyle !== 'none' && borderStyle !== 'hidden' &&
      Number.parseFloat(style[`border${side}Width`]) > 0 && alphaOf(style[`border${side}Color`]) > 0;
  });

// Shadows and outlines paint outside the border box; over-estimate their reach.
const extentOf = (style) => {
  let extent = 0;
  if (style.boxShadow !== 'none') {
    for (const match of style.boxShadow.matchAll(/(-?[\d.]+)px/g)) extent += Math.abs(Number.parseFloat(match[1]));
  }
  if (hasOutline(style)) {
    extent = Math.max(extent, Number.parseFloat(style.outlineWidth) +
      Math.max(0, Number.parseFloat(style.outlineOffset) || 0));
  }
  return extent;
};

const toRect = (rect) => ({ left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom });
const inflate = (rect, by) => (by > 0
  ? { left: rect.left - by, top: rect.top - by, right: rect.right + by, bottom: rect.bottom + by }
  : rect);
const clipTo = (rect, clip) => ({
  left: Math.max(rect.left, clip.left), top: Math.max(rect.top, clip.top),
  right: Math.min(rect.right, clip.right), bottom: Math.min(rect.bottom, clip.bottom),
});
const nonEmpty = (rect) => rect.right > rect.left && rect.bottom > rect.top;
const overlaps = (a, b) => Math.min(a.right, b.right) - Math.max(a.left, b.left) > OVERLAP &&
  Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) > OVERLAP;

const isFlexOrGrid = (display) => display.includes('flex') || display.includes('grid');
const containsAny = (value, keywords) => typeof value === 'string' &&
  value.split(/[\s,]+/).some((keyword) => keywords.includes(keyword));
const STACKING_WILL_CHANGE = ['opacity', 'transform', 'translate', 'rotate', 'scale', 'filter', 'backdrop-filter',
  'perspective', 'clip-path', 'mask', 'mask-image', 'isolation', 'mix-blend-mode'];
const CONTAINING_WILL_CHANGE = ['transform', 'translate', 'rotate', 'scale', 'filter', 'backdrop-filter', 'perspective'];
const PAINT_CONTAINMENT = ['layout', 'paint', 'strict', 'content'];

const hasTransform = (style) => style.transform !== 'none' || style.translate !== 'none' ||
  style.rotate !== 'none' || style.scale !== 'none' || (style.offsetPath !== undefined && style.offsetPath !== 'none');

// CSS stacking-context triggers as implemented by Chromium.
const createsStackingContext = (style, zApplies) =>
  style.position === 'fixed' || style.position === 'sticky' ||
  (zApplies && style.zIndex !== 'auto') ||
  Number.parseFloat(style.opacity) < 1 || style.mixBlendMode !== 'normal' ||
  hasTransform(style) || style.filter !== 'none' ||
  (style.backdropFilter !== undefined && style.backdropFilter !== 'none') ||
  style.perspective !== 'none' || style.clipPath !== 'none' ||
  (style.maskImage !== undefined && style.maskImage !== 'none') ||
  (style.webkitMaskImage !== undefined && style.webkitMaskImage !== 'none') ||
  style.isolation === 'isolate' || containsAny(style.contain, PAINT_CONTAINMENT) ||
  (style.containerType !== undefined && style.containerType !== 'normal') ||
  (style.viewTransitionName !== undefined && style.viewTransitionName !== 'none') ||
  containsAny(style.willChange, STACKING_WILL_CHANGE);

// Containing blocks decide which overflow clips apply to out-of-flow boxes.
// Only well-known triggers count, so an unknown trigger means less clipping
// (more possible overlap), never a missed overlap.
const containsFixed = (entry) => !entry.contents && (hasTransform(entry.style) ||
  entry.style.perspective !== 'none' || entry.style.filter !== 'none' ||
  (entry.style.backdropFilter !== undefined && entry.style.backdropFilter !== 'none') ||
  containsAny(entry.style.contain, PAINT_CONTAINMENT) ||
  (entry.style.containerType !== undefined && entry.style.containerType !== 'normal') ||
  containsAny(entry.style.willChange, CONTAINING_WILL_CHANGE));
const containsAbsolute = (entry) => !entry.contents && (entry.positioned || containsFixed(entry));

// Static paint-order guard. A CSS filter turns a non-positioned image into a
// stacking context painted with positioned, z-index auto/0 content in tree
// order (CSS 2.1 Appendix E step 6). Before that, the image paints with the
// in-flow content of its nearest positioned or stacking ancestor (its layer).
// The guard keeps an image sharp exactly when promotion would paint it over
// content drawn on top of it today:
// (a) in-flow content of the same layer that paints after the image (later
//     inline content, atomic boxes, or content inside later floats/atomics);
// (b) positioned or stacking content with z-index auto/0 that lies inside the
//     image's layer and precedes the image in tree order. Positive z-index
//     content stays above and negative stays below either way.
// Later block backgrounds paint beneath inline content already, so they are
// not overlap. SVG <image> content paints in document order regardless of
// filters, and already positioned/stacking images keep their order.
const createPaintOrderGuard = () => {
  const root = document.documentElement;
  const host = document.getElementById(options.hostId);
  const fullscreen = document.fullscreenElement;
  const rootStyle = getComputedStyle(root);
  // Body overflow is propagated to the viewport when the root's is visible.
  const bodyPropagates = rootStyle.overflowX === 'visible' && rootStyle.overflowY === 'visible';
  const entries = new Map();
  const texts = [];
  let position = 0;

  const inTopLayer = (node) => {
    if (node === fullscreen) return true;
    for (const selector of [':modal', ':popover-open']) {
      try {
        if (node.matches(selector)) return true;
      } catch (_) {
        // Older engines reject unknown pseudo-classes.
      }
    }
    return false;
  };

  const classify = (entry) => {
    const { style, parent } = entry;
    const display = style.display;
    entry.contents = display === 'contents';
    let boxParent = parent;
    while (boxParent !== null && boxParent.contents) boxParent = boxParent.parent;
    const outOfFlow = style.position === 'absolute' || style.position === 'fixed';
    entry.positioned = !entry.contents && style.position !== 'static';
    const flexItem = !entry.contents && !outOfFlow && boxParent !== null && isFlexOrGrid(boxParent.style.display);
    const zApplies = entry.positioned || flexItem;
    entry.z = zApplies && style.zIndex !== 'auto' ? Number.parseInt(style.zIndex, 10) || 0 : 0;
    const topLayer = !entry.pseudo && inTopLayer(entry.node);
    entry.topLayer = topLayer ? entry : parent !== null ? parent.topLayer : null;
    entry.stacking = !entry.contents &&
      ((entry.node === root && !entry.pseudo) || topLayer || createsStackingContext(style, zApplies));
    entry.floated = !entry.contents && !outOfFlow && style.float !== 'none';
    entry.replaced = !entry.pseudo && (REPLACED.has(entry.node.tagName) ||
      (entry.node instanceof SVGSVGElement && !entry.node.ownerSVGElement));
    // Atomic boxes (floats, inline-blocks, flex/grid items, replaced elements)
    // paint their whole subtree at once in their parent's float or inline phase.
    entry.atomic = !entry.contents && (entry.floated || flexItem || entry.replaced ||
      (display.startsWith('inline') && display !== 'inline'));
    entry.blockLevel = !entry.contents && !display.startsWith('inline');
    entry.transparent = (parent !== null && parent.transparent) || Number.parseFloat(style.opacity) === 0;
  };

  const childNodesOf = (node) => {
    if (node.shadowRoot) return [...node.shadowRoot.childNodes];
    if (node.localName === 'slot') {
      const assigned = node.assignedNodes({ flatten: true });
      return assigned.length > 0 ? assigned : [...node.childNodes];
    }
    return [...node.childNodes];
  };

  // Flex and grid items paint in order-modified document order.
  const paintOrder = (children) => {
    const keyed = children.map((child, index) => ({
      child, index,
      order: child.nodeType === Node.ELEMENT_NODE ? Number.parseInt(getComputedStyle(child).order, 10) || 0 : 0,
    }));
    if (keyed.every((entry) => entry.order === 0)) return children;
    return keyed.sort((a, b) => a.order - b.order || a.index - b.index).map((entry) => entry.child);
  };

  // Composed-tree pre-order positions: element, ::before, children, ::after.
  // An explicit stack keeps deeply nested documents from exhausting the call stack.
  const stack = [{ node: root, parent: null }];
  while (stack.length > 0) {
    const frame = stack.pop();
    if (frame.closing !== undefined) {
      frame.closing.after = position++;
      frame.closing.end = position;
      continue;
    }
    const { node, parent } = frame;
    if (node.nodeType === Node.TEXT_NODE) {
      if (parent !== null && /\S/.test(node.data)) texts.push({ node, parent, pos: position++ });
      continue;
    }
    if (node.nodeType !== Node.ELEMENT_NODE || node === host) continue;
    const style = getComputedStyle(node);
    if (style.display === 'none') continue;
    const entry = { node, parent, style, pseudo: false };
    classify(entry);
    entry.pos = position++;
    entry.before = position++;
    entries.set(node, entry);
    stack.push({ closing: entry });
    const leaf = entry.replaced || (node.ownerSVGElement !== null && node.ownerSVGElement !== undefined);
    if (!leaf && style.contentVisibility !== 'hidden') {
      let children = childNodesOf(node);
      if (isFlexOrGrid(style.display)) children = paintOrder(children);
      for (let index = children.length - 1; index >= 0; index--) stack.push({ node: children[index], parent: entry });
    }
  }

  const rootEntry = entries.get(root);
  if (rootEntry === undefined) return { allows: () => false };

  // Overflow clips, honouring containing blocks: absolutely positioned and
  // fixed boxes escape the clips of ancestors that do not contain them.
  for (const entry of entries.values()) {
    const { parent, style, node } = entry;
    entry.box = toRect(node.getBoundingClientRect());
    const own = parent === null ? UNBOUNDED
      : style.position === 'fixed' ? parent.clipFixed
        : style.position === 'absolute' ? parent.clipAbsolute : parent.clipFlow;
    entry.clip = own;
    let flow = own;
    const paintContained = containsAny(style.contain, ['paint', 'strict', 'content']);
    const clips = !entry.contents && node !== root && !(node === document.body && bodyPropagates);
    if (clips && (style.overflowX !== 'visible' || paintContained)) {
      flow = { ...flow, left: Math.max(flow.left, entry.box.left), right: Math.min(flow.right, entry.box.right) };
    }
    if (clips && (style.overflowY !== 'visible' || paintContained)) {
      flow = { ...flow, top: Math.max(flow.top, entry.box.top), bottom: Math.min(flow.bottom, entry.box.bottom) };
    }
    entry.clipFlow = flow;
    entry.clipAbsolute = containsAbsolute(entry) ? flow : parent === null ? UNBOUNDED : parent.clipAbsolute;
    entry.clipFixed = containsFixed(entry) ? flow : parent === null ? UNBOUNDED : parent.clipFixed;
  }

  const items = [];
  const addItem = (owner, kind, pos, rects) => {
    const visible = rects.filter(nonEmpty);
    if (visible.length > 0) items.push({ owner, kind, pos, rects: visible });
  };

  const containerOf = (entry, contains) => {
    for (let current = entry; current !== null; current = current.parent) {
      if (contains(current)) return current;
    }
    return null;
  };

  // Generated boxes have no DOM rects; derive positioned ones from resolved
  // offsets and fall back to the originating box (or containing block).
  const pseudoRect = (entry, style) => {
    if (style.position !== 'absolute' && style.position !== 'fixed') return entry.box;
    const container = style.position === 'fixed' ? containerOf(entry, containsFixed) : containerOf(entry, containsAbsolute);
    const fallback = container === null ? entry.box : {
      left: Math.min(container.box.left, entry.box.left), top: Math.min(container.box.top, entry.box.top),
      right: Math.max(container.box.right, entry.box.right), bottom: Math.max(container.box.bottom, entry.box.bottom),
    };
    const px = (value) => (/^-?[\d.]+(?:e[-+]?\d+)?px$/.test(value) ? Number.parseFloat(value) : null);
    const [left, top, width, height] = [style.left, style.top, style.width, style.height].map(px);
    if ([left, top, width, height].includes(null)) return fallback;
    let originLeft = 0;
    let originTop = 0;
    if (container !== null) {
      originLeft = container.box.left + container.node.clientLeft;
      originTop = container.box.top + container.node.clientTop;
    } else if (style.position === 'absolute') {
      originLeft = -window.scrollX;
      originTop = -window.scrollY;
    }
    const sum = (...values) => values.reduce((total, value) => total + (Number.parseFloat(value) || 0), 0);
    const extraWidth = style.boxSizing === 'border-box' ? 0
      : sum(style.paddingLeft, style.paddingRight, style.borderLeftWidth, style.borderRightWidth);
    const extraHeight = style.boxSizing === 'border-box' ? 0
      : sum(style.paddingTop, style.paddingBottom, style.borderTopWidth, style.borderBottomWidth);
    const x = originLeft + left + sum(style.marginLeft);
    const y = originTop + top + sum(style.marginTop);
    return { left: x, top: y, right: x + width + extraWidth, bottom: y + height + extraHeight };
  };

  for (const entry of entries.values()) {
    if (entry.contents) continue;
    if ((entry.replaced || paintsBox(entry.style)) && entry.style.visibility === 'visible' && !entry.transparent) {
      const extent = extentOf(entry.style);
      addItem(entry, entry.replaced ? 'replaced' : 'box', entry.pos,
        [...entry.node.getClientRects()].map((rect) => clipTo(inflate(toRect(rect), extent), entry.clip)));
    }
    if (entry.replaced || entry.transparent) continue;
    for (const which of ['::before', '::after']) {
      const style = getComputedStyle(entry.node, which);
      if (style.display === 'none' || style.content === 'none' || style.content === 'normal') continue;
      if (style.visibility !== 'visible' || Number.parseFloat(style.opacity) === 0) continue;
      const generatesText = style.content !== '""' && style.content !== "''";
      if (!generatesText && !paintsBox(style)) continue;
      const pseudo = { node: entry.node, parent: entry, style, pseudo: true };
      classify(pseudo);
      pseudo.pos = which === '::before' ? entry.before : entry.after;
      pseudo.end = pseudo.pos + 1;
      const clip = style.position === 'fixed' ? entry.clipFixed
        : style.position === 'absolute' ? entry.clipAbsolute : entry.clipFlow;
      addItem(pseudo, generatesText ? 'text' : 'box', pseudo.pos,
        [clipTo(inflate(pseudoRect(entry, style), extentOf(style)), clip)]);
    }
  }
  for (const text of texts) {
    const parent = text.parent;
    if (parent.style.visibility !== 'visible' || parent.transparent) continue;
    const range = document.createRange();
    range.selectNodeContents(text.node);
    addItem(parent, 'text', text.pos, [...range.getClientRects()].map((rect) => clipTo(toRect(rect), parent.clipFlow)));
  }
  for (const item of items) {
    item.bounds = item.rects.reduce((bounds, rect) => ({
      left: Math.min(bounds.left, rect.left), top: Math.min(bounds.top, rect.top),
      right: Math.max(bounds.right, rect.right), bottom: Math.max(bounds.bottom, rect.bottom),
    }));
  }

  const contains = (ancestor, entry) => ancestor.pos < entry.pos && entry.pos < ancestor.end;
  const ownPhase = (item) => (item.kind === 'box' && item.owner.blockLevel && !hasOutline(item.owner.style) ? 3 : 5);
  const atomicPhase = (entry) => (entry.floated ? 4 : 5);

  // Whether an in-flow item of the image's layer paints after the image now.
  // Compare inside the deepest atomic box containing both: floats paint before
  // inline-level content, then tree (paint) order decides.
  const paintsAfter = (item, chain, layer) => {
    let shared = -1;
    let outermost = null;
    for (let current = item.owner; current !== null && current !== layer; current = current.parent) {
      const index = chain.indexOf(current);
      if (index !== -1 && index < chain.length - 1) {
        shared = index;
        break;
      }
      if (current.atomic) outermost = current;
    }
    const imageSide = chain[shared + 1];
    const imageKey = [atomicPhase(imageSide), imageSide.pos];
    const itemKey = outermost !== null ? [atomicPhase(outermost), outermost.pos] : [ownPhase(item), item.pos];
    return itemKey[0] > imageKey[0] || (itemKey[0] === imageKey[0] && itemKey[1] > imageKey[1]);
  };

  const allows = (image) => {
    if (image.ownerSVGElement) return true;
    const target = entries.get(image);
    // Unreachable in the composed tree (for example inside <foreignObject>): keep sharp.
    if (target === undefined) return false;
    if (target.positioned || target.stacking) return true;
    let stacking = target.parent;
    while (stacking !== null && !stacking.stacking) stacking = stacking.parent;
    if (stacking === null) stacking = rootEntry;
    let layer = target.parent;
    while (layer !== null && !layer.stacking && !layer.positioned) layer = layer.parent;
    if (layer === null) layer = rootEntry;
    const imageRect = clipTo(target.box, target.clip);
    if (!nonEmpty(imageRect)) return true;
    // Atomic ancestors of the image below its layer, outermost first, then the image.
    const chain = [];
    for (let current = target.parent; current !== null && current !== layer; current = current.parent) {
      if (current.atomic) chain.unshift(current);
    }
    chain.push(target);
    for (const item of items) {
      if (item.owner === target || !overlaps(item.bounds, imageRect)) continue;
      if (!item.rects.some((rect) => overlaps(rect, imageRect))) continue;
      // Top-layer content paints above everything outside its own top layer.
      if (item.owner.topLayer !== target.topLayer) continue;
      // An ancestor's own background and borders paint beneath its descendants.
      if (item.kind !== 'text' && !item.owner.pseudo && contains(item.owner, target)) continue;
      let outermostStacking = null;
      let nearestPositioned = null;
      let inside = false;
      for (let current = item.owner; current !== null; current = current.parent) {
        if (current === stacking) {
          inside = true;
          break;
        }
        if (current.stacking) outermostStacking = current;
        else if (current.positioned && nearestPositioned === null) nearestPositioned = current;
      }
      // Promotion stays inside the image's stacking context, so content outside it keeps its order.
      if (!inside) continue;
      if (outermostStacking !== null) {
        if (outermostStacking.z !== 0) continue;
        if (layer.pos < outermostStacking.pos && outermostStacking.pos < target.pos) return false;
        continue;
      }
      if (nearestPositioned !== null && nearestPositioned !== layer) {
        if (layer.pos < nearestPositioned.pos && nearestPositioned.pos < target.pos) return false;
        continue;
      }
      // In-flow content of the stacking context stays beneath an image whose layer is positioned.
      if (nearestPositioned === null && layer !== stacking) continue;
      if (paintsAfter(item, chain, layer)) return false;
    }
    return true;
  };
  return { allows };
};

// Applies the guard to qualifying images; flags each as PIXELATE or SHARP.
const applyGuard = (eligible) => {
  const result = { simplified: 0, overlapped: 0, lost: 0 };
  if (eligible.length === 0) return result;
  for (const element of eligible) element.setAttribute(ATTRIBUTE, SHARP);
  const guard = createPaintOrderGuard();
  const promoted = [];
  for (const element of eligible) {
    if (guard.allows(element)) promoted.push(element);
    else result.overlapped++;
  }
  for (const element of promoted) element.setAttribute(ATTRIBUTE, PIXELATE);
  for (const element of promoted) {
    // Page rules can outrank the filter (cascade layers, more specific
    // !important rules, discrete transitions); never count an unfiltered image.
    if (getComputedStyle(element).filter.includes(options.filterId)) {
      result.simplified++;
    } else {
      element.removeAttribute(ATTRIBUTE);
      result.lost++;
    }
  }
  return result;
};

const mark = () => {
  const counts = { simplified: 0, small: 0, excluded: 0, filtered: 0, transformed: 0, overlapped: 0, invalidSelectors: 0 };
  clear();
  if (!ensure()) return counts;
  // Invalid selectors match nothing, like invalid hideSelectors CSS rules;
  // only their number is reported, never their text.
  const selectors = [];
  for (const selector of options.excludeSelectors) {
    try {
      document.documentElement.matches(selector);
      selectors.push(selector);
    } catch (_) {
      counts.invalidSelectors++;
    }
  }
  const eligible = [];
  for (const element of document.querySelectorAll('img, video, svg image')) {
    const style = getComputedStyle(element);
    if (style.display === 'none' || style.visibility !== 'visible' || element.getClientRects().length === 0) continue;
    const rect = element.getBoundingClientRect();
    if (rect.width < options.minSize || rect.height < options.minSize) counts.small++;
    else if (selectors.some((selector) => element.closest(selector) !== null)) counts.excluded++;
    else if (style.filter !== 'none') counts.filtered++;
    else if (!pixelAligned(element)) counts.transformed++;
    else eligible.push(element);
  }
  const result = applyGuard(eligible);
  counts.simplified = result.simplified;
  counts.overlapped = result.overlapped;
  counts.filtered += result.lost;
  return counts;
};

const refresh = () => {
  if (!ensure()) {
    clear();
    return { simplified: 0, overlapped: 0, filtered: 0 };
  }
  const result = applyGuard([...document.querySelectorAll(`[${ATTRIBUTE}]`)]);
  return { simplified: result.simplified, overlapped: result.overlapped, filtered: result.lost };
};

switch (operation) {
  case 'ensure':
    return { ensured: ensure() };
  case 'mark':
    return mark();
  case 'refresh':
    return refresh();
  case 'clear':
    return { cleared: clear() };
  default:
    throw new Error('Unknown image simplification operation');
}
