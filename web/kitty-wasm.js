let modulePromise

async function loadModule() {
  if (!modulePromise) {
    modulePromise = WebAssembly.instantiateStreaming(fetch("/vendor/ghostty-vt-kitty.wasm")).then(({ instance }) => {
      const e = instance.exports
      const memory = e.memory
      const bytes = () => new Uint8Array(memory.buffer)
      const ptr = e.ghostty_type_json()
      let end = ptr
      while (bytes()[end]) end++
      const abi = JSON.parse(new TextDecoder().decode(bytes().subarray(ptr, end)))
      const enums = Object.fromEntries(
        Object.entries(abi.types).filter(([, type]) => type.kind === "enum").map(([name, type]) => [name, type.values]),
      )
      return { e, memory, enums }
    })
  }
  return modulePromise
}

export async function createKittyWasmFactory() {
  const module = await loadModule()
  return (cols, rows, host) => new KittyWasmPane(module, cols, rows, host)
}

class KittyWasmPane {
  constructor(module, cols, rows, host) {
    this.module = module
    this.cellWidth = 16
    this.cellHeight = 16
    this.cols = cols
    this.rows = rows
    this.clock = null
    this.canvas = document.createElement("canvas")
    this.canvas.className = "kitty-wasm-layer"
    Object.assign(this.canvas.style, { position: "absolute", inset: "0", width: "100%", height: "100%", pointerEvents: "none", zIndex: "4" })
    host.style.position = "relative"
    host.append(this.canvas)
    this.initialize()
  }

  get e() { return this.module.e }
  get memory() { return this.module.memory }
  get enums() { return this.module.enums }
  bytes() { return new Uint8Array(this.memory.buffer) }
  view() { return new DataView(this.memory.buffer) }
  check(result, name) { if (result !== 0) throw new Error(`${name}: ${result}`) }
  alloc(size) { const ptr = this.e.ghostty_wasm_alloc(size); if (!ptr) throw new Error(`WASM allocation failed: ${size}`); return ptr }
  allocHandle(make, ...args) {
    const slot = this.e.ghostty_wasm_alloc_opaque()
    this.check(make(0, slot, ...args), "handle constructor")
    const handle = this.e.ghostty_wasm_take_opaque(slot)
    this.e.ghostty_wasm_free_opaque(slot)
    if (!handle) throw new Error("null WASM handle")
    return handle
  }

  initialize() {
    this.term = this.allocHandle(this.e.ghostty_terminal_new, this.cols, this.rows)
    this.check(this.e.ghostty_terminal_resize(this.term, this.cols, this.rows, this.cellWidth, this.cellHeight), "resize")
    const limit = this.alloc(8)
    this.view().setBigUint64(limit, 64n * 1024n * 1024n, true)
    this.check(this.e.ghostty_terminal_set(this.term, this.enums.GhosttyTerminalOption.KITTY_IMAGE_STORAGE_LIMIT, limit), "image storage")
    this.e.ghostty_wasm_free(limit, 8)
    this.state = this.allocHandle(this.e.ghostty_render_state_new)
    this.iterator = this.allocHandle(this.e.ghostty_render_state_image_iterator_new)
    this.scratch = this.alloc(16)
    this.canvas.width = this.cols * this.cellWidth
    this.canvas.height = this.rows * this.cellHeight
  }

  destroyHandles() {
    if (!this.term) return
    this.e.ghostty_wasm_free(this.scratch, 16)
    this.e.ghostty_render_state_image_iterator_free(this.iterator)
    this.e.ghostty_render_state_free(this.state)
    this.e.ghostty_terminal_free(this.term)
    this.term = 0
  }

  reset() {
    this.destroyHandles()
    this.clock = null
    this.initialize()
  }

  dispose() {
    this.destroyHandles()
    this.canvas.remove()
  }

  write(value) {
    const input = typeof value === "string" ? new TextEncoder().encode(value) : value
    if (!input.byteLength) return
    const ptr = this.alloc(input.byteLength)
    this.bytes().set(input, ptr)
    this.e.ghostty_terminal_vt_write(this.term, ptr, input.byteLength)
    this.e.ghostty_wasm_free(ptr, input.byteLength)
  }

  resize(cols, rows) {
    this.cols = cols
    this.rows = rows
    this.check(this.e.ghostty_terminal_resize(this.term, cols, rows, this.cellWidth, this.cellHeight), "resize")
    this.canvas.width = cols * this.cellWidth
    this.canvas.height = rows * this.cellHeight
  }

  terminalGet(key) {
    this.bytes().fill(0, this.scratch, this.scratch + 16)
    this.check(this.e.ghostty_terminal_get(this.term, key, this.scratch), `terminal get ${key}`)
    return this.view().getUint32(this.scratch, true)
  }

  stateGet(key, wide = false) {
    this.bytes().fill(0, this.scratch, this.scratch + 16)
    if (key === this.enums.GhosttyRenderStateData.IMAGE_ITERATOR) this.view().setUint32(this.scratch, this.iterator, true)
    this.check(this.e.ghostty_render_state_get(this.state, key, this.scratch), `render state ${key}`)
    return wide ? this.view().getBigUint64(this.scratch, true) : this.view().getUint32(this.scratch, true)
  }

  placementGet(key, signed = false, wide = false) {
    this.bytes().fill(0, this.scratch, this.scratch + 16)
    this.check(this.e.ghostty_render_state_image_get(this.iterator, key, this.scratch), `placement ${key}`)
    if (wide) return this.view().getBigUint64(this.scratch, true)
    return signed ? this.view().getInt32(this.scratch, true) : this.view().getUint32(this.scratch, true)
  }

  storedImage(graphics, id) {
    const image = this.e.ghostty_kitty_graphics_image(graphics, id)
    if (!image) return null
    const get = (key, kind = "u32") => {
      this.bytes().fill(0, this.scratch, this.scratch + 16)
      if (this.e.ghostty_kitty_graphics_image_get(image, key, this.scratch) !== 0) return null
      if (kind === "string") {
        const ptr = this.view().getUint32(this.scratch, true)
        const len = this.view().getUint32(this.scratch + 4, true)
        return this.bytes().slice(ptr, ptr + len)
      }
      return this.view().getUint32(this.scratch, true)
    }
    return {
      width: get(this.enums.GhosttyKittyGraphicsImageData.WIDTH),
      height: get(this.enums.GhosttyKittyGraphicsImageData.HEIGHT),
      format: get(this.enums.GhosttyKittyGraphicsImageData.FORMAT),
      pixels: get(this.enums.GhosttyKittyGraphicsImageData.PIXELS, "string"),
    }
  }

  rgba(image) {
    if (image.format === this.enums.GhosttyKittyImageFormat.RGBA) return image.pixels
    const output = new Uint8ClampedArray(image.width * image.height * 4)
    if (image.format === this.enums.GhosttyKittyImageFormat.RGB) {
      for (let s = 0, d = 0; s < image.pixels.length; s += 3, d += 4) output.set([image.pixels[s], image.pixels[s + 1], image.pixels[s + 2], 255], d)
    } else if (image.format === this.enums.GhosttyKittyImageFormat.GRAY_ALPHA) {
      for (let s = 0, d = 0; s < image.pixels.length; s += 2, d += 4) output.set([image.pixels[s], image.pixels[s], image.pixels[s], image.pixels[s + 1]], d)
    } else if (image.format === this.enums.GhosttyKittyImageFormat.GRAY) {
      for (let s = 0, d = 0; s < image.pixels.length; s++, d += 4) output.set([image.pixels[s], image.pixels[s], image.pixels[s], 255], d)
    }
    return output
  }

  tick(atMs) {
    const graphics = this.terminalGet(this.enums.GhosttyTerminalData.KITTY_GRAPHICS)
    if (this.clock === null || atMs < this.clock) this.clock = atMs
    while (this.clock + 8 < atMs) {
      this.clock += 8
      this.view().setBigUint64(this.scratch, 0n, true)
      this.e.ghostty_kitty_graphics_animation_tick(graphics, BigInt(this.clock), this.scratch)
    }
    this.clock = atMs
    this.view().setBigUint64(this.scratch, 0n, true)
    this.e.ghostty_kitty_graphics_animation_tick(graphics, BigInt(atMs), this.scratch)
    return graphics
  }

  render(atMs) {
    const graphics = this.tick(Math.max(0, Math.floor(atMs)))
    this.check(this.e.ghostty_render_state_update(this.state, this.term), "render update")
    this.stateGet(this.enums.GhosttyRenderStateData.IMAGE_ITERATOR)
    const placements = []
    while (this.e.ghostty_render_state_image_iterator_next(this.iterator)) {
      placements.push({
        id: this.placementGet(this.enums.GhosttyRenderStateImageData.IMAGE_ID),
        layer: this.placementGet(this.enums.GhosttyRenderStateImageData.LAYER),
        z: this.placementGet(this.enums.GhosttyRenderStateImageData.Z, true),
        col: this.placementGet(this.enums.GhosttyRenderStateImageData.VIEWPORT_COL, true),
        row: this.placementGet(this.enums.GhosttyRenderStateImageData.VIEWPORT_ROW, true),
        xOffset: this.placementGet(this.enums.GhosttyRenderStateImageData.CELL_OFFSET_X),
        yOffset: this.placementGet(this.enums.GhosttyRenderStateImageData.CELL_OFFSET_Y),
        width: this.placementGet(this.enums.GhosttyRenderStateImageData.PIXEL_WIDTH),
        height: this.placementGet(this.enums.GhosttyRenderStateImageData.PIXEL_HEIGHT),
        sourceX: this.placementGet(this.enums.GhosttyRenderStateImageData.SOURCE_X),
        sourceY: this.placementGet(this.enums.GhosttyRenderStateImageData.SOURCE_Y),
        sourceWidth: this.placementGet(this.enums.GhosttyRenderStateImageData.SOURCE_WIDTH),
        sourceHeight: this.placementGet(this.enums.GhosttyRenderStateImageData.SOURCE_HEIGHT),
      })
    }
    placements.sort((a, b) => a.layer - b.layer || a.z - b.z)
    const context = this.canvas.getContext("2d")
    context.clearRect(0, 0, this.canvas.width, this.canvas.height)
    for (const placement of placements) {
      const image = this.storedImage(graphics, placement.id)
      if (!image?.pixels || !image.width || !image.height) continue
      const source = document.createElement("canvas")
      source.width = image.width
      source.height = image.height
      const sourceContext = source.getContext("2d")
      const imageData = sourceContext.createImageData(image.width, image.height)
      imageData.data.set(this.rgba(image))
      sourceContext.putImageData(imageData, 0, 0)
      context.drawImage(
        source,
        placement.sourceX, placement.sourceY, placement.sourceWidth, placement.sourceHeight,
        placement.col * this.cellWidth + placement.xOffset,
        placement.row * this.cellHeight + placement.yOffset,
        placement.width, placement.height,
      )
    }
    this.e.ghostty_render_state_clean(this.state)
  }
}
