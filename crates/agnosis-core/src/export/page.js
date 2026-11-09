// The self-contained page's script (roadmap Extension 4 §2.3).
//
// The page is a canvas, a blob decoder and a timer. It re-creates the
// parade from the recorded frames — there is no simulation here, so the
// page cannot drift from the app: the frames ARE the app's frames.
//
// The decoder is the JavaScript twin of `src/export/codec.rs`; the two
// must agree, and the Rust side is what the tests check the blob
// against.
(function () {
  "use strict";

  var blobNode = document.getElementById("agnosis-blob");
  var manifestNode = document.getElementById("agnosis-manifest");
  var canvas = document.getElementById("stage");
  var scrub = document.getElementById("scrub");
  var play = document.getElementById("play");

  var manifest = {};
  try {
    manifest = JSON.parse(manifestNode.textContent);
  } catch (e) {
    manifest = { name: "a parade" };
  }

  function base64ToBytes(text) {
    var binary = atob(text);
    var bytes = new Uint8Array(binary.length);
    for (var i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  }

  // The ceiling on the cells of one frame. The cells are decoded from
  // diffs, so the blob's size does not bound them (a 17 kB blob decodes
  // into 345 600 cells per frame): the bound has to be a number the
  // format itself keeps small. The row-budget grid is about 960 x 360,
  // so this is generous — and far below the 17 GB a hostile
  // 65535 x 65535 blob would ask for (audit 2026-10-21 §F13).
  var MAX_CELLS_PER_FRAME = 4 * 1024 * 1024;

  // The ceiling on the cells of a whole blob. The decoder keeps every
  // frame, so a real export is 240 frames of a 960 x 360 grid — 83 M
  // cells — and a hostile blob must not ask for more than that (the
  // Rust twin's MAX_DECODED_CELLS, audit 2026-10-23 F-150).
  var MAX_DECODED_CELLS = 128 * 1024 * 1024;

  function Reader(bytes) {
    this.bytes = bytes;
    this.at = 0;
  }
  Reader.prototype.u8 = function () {
    if (this.at >= this.bytes.length) {
      throw new Error("the blob ends in the middle of a byte");
    }
    return this.bytes[this.at++];
  };
  Reader.prototype.u16 = function () {
    var low = this.u8();
    var high = this.u8();
    return low | (high << 8);
  };
  Reader.prototype.varint = function () {
    var value = 0;
    var shift = 0;
    var byte;
    do {
      if (shift > 28) {
        throw new Error("the blob carries a varint that is too long");
      }
      byte = this.u8();
      value += (byte & 0x7f) * Math.pow(2, shift);
      shift += 7;
    } while (byte & 0x80);
    return value;
  };
  Reader.prototype.unzigzag = function (value) {
    return (value >>> 1) ^ -(value & 1);
  };
  // The bytes of a run of `length` bytes, or a throw when the blob is
  // shorter than that.
  Reader.prototype.take = function (length) {
    if (length < 0 || this.at + length > this.bytes.length) {
      throw new Error("the blob ends before the data it promises");
    }
    var slice = this.bytes.subarray(this.at, this.at + length);
    this.at += length;
    return slice;
  };

  // The head of the blob: the geometry, the palette, the glyphs and the
  // backdrop layers.
  function readHead(reader) {
    var magic = String.fromCharCode(
      reader.bytes[0],
      reader.bytes[1],
      reader.bytes[2],
      reader.bytes[3]
    );
    if (magic !== "DKSF") {
      throw new Error("this file is not an agnosis frame blob");
    }
    reader.at = 4;
    var version = reader.u8();
    if (version !== 2) {
      throw new Error("this blob is version " + version + ", the page speaks 2");
    }
    var flags = reader.u8();
    var head = {
      hasSky: (flags & 1) !== 0,
      cols: reader.u16(),
      rows: reader.u16(),
      cellW: reader.u16(),
      cellH: reader.u16(),
      ground: reader.u16(),
      fps: reader.u16()
    };
    var paletteLen = reader.u16();
    var frameCount = reader.u16();
    var glyphCount = reader.u16();
    var layerCount = reader.u16();
    head.frameCount = frameCount;
    checkTheGeometry(head, frameCount, reader.bytes.length);

    // The message band (blob version 2, audit 2026-10-23 F-140): a flag
    // byte right after the counts — the same slot the Rust writer uses —
    // then the band cropped to its ink. The page paints it once, at
    // offset 0, over everything else: the message is chrome.
    var messageFlag = reader.u8();
    head.message = null;
    if (messageFlag !== 0) {
      var msgRow = reader.u16();
      var msgCol = reader.u16();
      var msgCols = reader.u16();
      var msgRows = reader.u16();
      head.message = {
        row: msgRow,
        col: msgCol,
        cols: msgCols,
        rows: msgRows,
        cells: reader.take(msgCols * msgRows)
      };
    }

    head.palette = [];
    for (var i = 0; i < paletteLen; i++) {
      var glyph = reader.u8();
      head.palette.push({
        glyph: glyph,
        color: [reader.u8(), reader.u8(), reader.u8()]
      });
    }

    head.glyphs = [];
    for (var g = 0; g < glyphCount; g++) {
      var code = reader.u8();
      var w = reader.u16();
      var h = reader.u16();
      head.glyphs.push({ code: code, w: w, h: h, coverage: reader.take(w * h) });
    }

    head.layers = [];
    for (var l = 0; l < layerCount; l++) {
      var cols = reader.u16();
      var rows = reader.u16();
      var rowOffset = reader.u16();
      var drawnRows = reader.u16();
      var rate = reader.u16();
      head.layers.push({
        cols: cols,
        rows: rows,
        rowOffset: rowOffset,
        drawnRows: drawnRows,
        rate: rate,
        cells: reader.take(cols * rows)
      });
    }
    return head;
  }

  // The geometry a blob may claim. A hostile blob could claim 65535 x
  // 65535 cells — an allocation of about 17 GB in the browser tab
  // (audit 2026-10-21 §F13).
  function checkTheGeometry(head, frameCount, blobBytes) {
    var cells = head.cols * head.rows;
    if (!isFinite(cells) || cells > MAX_CELLS_PER_FRAME) {
      throw new Error(
        "this blob claims a " + head.cols + " x " + head.rows + " grid, which is impossible"
      );
    }
    // The decoder keeps every frame, so the whole blob's cells are
    // bounded too — the Rust twin's MAX_DECODED_CELLS (audit 2026-10-23
    // F-150).
    var total = cells * frameCount;
    if (!isFinite(total) || total > MAX_DECODED_CELLS) {
      throw new Error(
        "this blob decodes into " + total + " cells, which is impossible"
      );
    }
    // Every row of every frame takes at least one byte, so a blob that
    // claims more rows than it has bytes is malformed.
    if (head.rows * frameCount > blobBytes) {
      throw new Error("this blob does not carry the frames it claims");
    }
  }

  // The player: it walks the frames and keeps the current grid.
  function Player(bytes) {
    this.reader = new Reader(bytes);
    this.head = readHead(this.reader);
    this.framesStart = this.reader.at;
    this.previous = new Uint16Array(this.head.cols * this.head.rows);
    this.current = new Uint16Array(this.head.cols * this.head.rows);
    this.index = -1;
    this.sky = null;
    this.offsetPx = 0;
    this.layerOffsets = [];
  }

  Player.prototype.reset = function () {
    this.reader = new Reader(this.reader.bytes);
    this.reader.at = this.framesStart;
    this.previous.fill(0);
    this.current.fill(0);
    this.index = -1;
    this.sky = null;
  };

  // Decode the next frame and apply it to the grid.
  Player.prototype.step = function () {
    var head = this.head;
    var reader = this.reader;
    if (this.index + 1 >= head.frameCount) {
      return false;
    }
    this.offsetPx = reader.unzigzag(reader.varint());
    var sameSky = reader.u8() !== 0;
    if (!sameSky) {
      this.sky = [];
      for (var s = 0; s < head.rows; s++) {
        this.sky.push([reader.u8(), reader.u8(), reader.u8()]);
      }
    } else if (this.sky === null && head.hasSky) {
      // The blob's header promised a sky, and the very first frame
      // claims to repeat one — but no frame carried it. The Rust twin
      // refuses the same blob (codec.rs `take_frame`).
      throw new Error("this blob repeats a sky it never sent");
    }
    this.layerOffsets = [];
    for (var l = 0; l < head.layers.length; l++) {
      this.layerOffsets.push(reader.unzigzag(reader.varint()));
    }
    this.current.set(this.previous);
    for (var row = 0; row < head.rows; row++) {
      var mode = reader.u8();
      if (mode === 4) {
        // The row kept the previous frame's cells.
        continue;
      }
      var start = row * head.cols;
      if (mode === 1) {
        // The base is the row above, in the same frame.
        if (row === 0) {
          throw new Error("this blob's first row claims a row above it");
        }
        this.current.copyWithin(start, start - head.cols, start);
      } else if (mode === 2) {
        // The base is the previous frame's row, moved sideways.
        var shift = reader.unzigzag(reader.varint());
        for (var c = 0; c < head.cols; c++) {
          var source = c + shift;
          this.current[start + c] =
            source >= 0 && source < head.cols ? this.previous[start + source] : 0;
        }
      } else if (mode === 3) {
        // The base is the row above, moved sideways.
        if (row === 0) {
          throw new Error("this blob's first row claims a row above it");
        }
        var aboveShift = reader.unzigzag(reader.varint());
        for (var a = 0; a < head.cols; a++) {
          var from = a + aboveShift;
          this.current[start + a] =
            from >= 0 && from < head.cols ? this.current[start - head.cols + from] : 0;
        }
      }
      var runCount = reader.varint();
      var previousEnd = 0;
      for (var run = 0; run < runCount; run++) {
        var delta = reader.varint();
        var len = reader.varint();
        var value = reader.u8();
        var from = previousEnd + delta;
        previousEnd = from + len;
        if (from < 0 || from + len > head.cols) {
          // The Rust twin refuses the same blob: garbage in, an error
          // card out — not a painted-over frame (audit 2026-10-22
          // §P2-16).
          throw new Error("this blob carries a run that runs past the row");
        }
        this.current.fill(value, start + from, start + from + len);
      }
    }
    this.previous.set(this.current);
    this.index += 1;
    return true;
  };

  // Move to a frame, rebuilding the grid when the target is behind us.
  Player.prototype.seek = function (target) {
    if (target < this.index) {
      this.reset();
    }
    while (this.index < target) {
      if (!this.step()) {
        break;
      }
    }
  };

  function mod(value, size) {
    if (size <= 0) {
      return 0;
    }
    return ((value % size) + size) % size;
  }

  function rgb(color) {
    return "rgb(" + color[0] + "," + color[1] + "," + color[2] + ")";
  }

  var bytes;
  var player;
  var sprites = [];
  var context;
  var playing = true;
  var accumulator = 0;
  var lastTime = 0;

  function buildSprites(head) {
    var byCode = {};
    for (var g = 0; g < head.glyphs.length; g++) {
      byCode[head.glyphs[g].code] = head.glyphs[g];
    }
    // The coverage **floor** (blit.rs `FLOOR_CELL_MAX`): at a small cell
    // any nonzero coverage blends as full ink. Without it a glyph at the
    // export's 2x3 cell covers 2 of 6 px and reads at a third of the
    // app's brightness (audit 2026-10-22 §P2-17).
    var floorCellMax = 6;
    var applyFloor = head.cellW <= floorCellMax && head.cellH <= floorCellMax;
    for (var p = 0; p < head.palette.length; p++) {
      var entry = head.palette[p];
      var bitmap = byCode[entry.glyph];
      if (!bitmap) {
        sprites.push(null);
        continue;
      }
      var sprite = document.createElement("canvas");
      sprite.width = Math.max(1, bitmap.w);
      sprite.height = Math.max(1, bitmap.h);
      var g2 = sprite.getContext("2d");
      var image = g2.createImageData(sprite.width, sprite.height);
      for (var i = 0; i < bitmap.coverage.length; i++) {
        image.data[i * 4] = entry.color[0];
        image.data[i * 4 + 1] = entry.color[1];
        image.data[i * 4 + 2] = entry.color[2];
        var coverage = bitmap.coverage[i];
        image.data[i * 4 + 3] =
          applyFloor && coverage > 0 ? 255 : coverage;
      }
      g2.putImageData(image, 0, 0);
      sprites.push(sprite);
    }
  }

  function draw() {
    var head = player.head;
    var width = head.cols * head.cellW;
    var height = head.rows * head.cellH;

    if (player.sky) {
      for (var row = 0; row < head.rows; row++) {
        context.fillStyle = rgb(player.sky[row]);
        context.fillRect(0, row * head.cellH, width, head.cellH);
      }
    } else {
      context.fillStyle = "#000000";
      context.fillRect(0, 0, width, height);
    }

    // The backdrop layers, back to front, each with its own scroll.
    for (var l = 0; l < head.layers.length; l++) {
      var layer = head.layers[l];
      var offset = player.layerOffsets[l] || 0;
      var from = Math.max(0, Math.floor(-offset / head.cellW));
      var to = Math.min(
        head.cols + layer.cols,
        Math.ceil((width - offset) / head.cellW)
      );
      for (var r = 0; r < layer.drawnRows && r < layer.rows; r++) {
        var y = (r + layer.rowOffset) * head.cellH;
        for (var col = from; col < to; col++) {
          var index = layer.cells[mod(col, layer.cols) + r * layer.cols];
          if (index === 0) {
            continue;
          }
          var sprite = sprites[index];
          if (sprite) {
            context.drawImage(sprite, col * head.cellW + offset, y);
          }
        }
      }
    }

    // The scene grid: the parade rows scroll, the rest does not.
    for (var srow = 0; srow < head.rows; srow++) {
      var shift = srow < head.ground ? player.offsetPx : 0;
      var base = srow * head.cols;
      for (var scol = 0; scol < head.cols; scol++) {
        var cell = player.current[base + scol];
        if (cell === 0) {
          continue;
        }
        var art = sprites[cell];
        if (art) {
          context.drawImage(art, scol * head.cellW + shift, srow * head.cellH);
        }
      }
    }

    // The message band, **last** (audit 2026-10-23 F-140): it is chrome,
    // drawn over the parade at offset 0, once — no wrap, no scroll.
    if (head.message) {
      for (var mrow = 0; mrow < head.message.rows; mrow++) {
        var mbase = mrow * head.message.cols;
        for (var mcol = 0; mcol < head.message.cols; mcol++) {
          var mindex = head.message.cells[mbase + mcol];
          if (mindex === 0) {
            continue;
          }
          var mart = sprites[mindex];
          if (mart) {
            context.drawImage(
              mart,
              (head.message.col + mcol) * head.cellW,
              (head.message.row + mrow) * head.cellH
            );
          }
        }
      }
    }
  }

  function showFrame(index) {
    player.seek(index);
    scrub.value = String(player.index);
    draw();
  }

  function loop(now) {
    var delta = lastTime ? now - lastTime : 0;
    lastTime = now;
    if (playing) {
      accumulator += delta;
      var frameTime = 1000 / player.head.fps;
      var advanced = false;
      while (accumulator >= frameTime) {
        accumulator -= frameTime;
        if (!player.step()) {
          player.reset();
          player.step();
        }
        advanced = true;
      }
      if (advanced) {
        scrub.value = String(player.index);
        draw();
      }
    }
    requestAnimationFrame(loop);
  }

  function start() {
    try {
      bytes = base64ToBytes(blobNode.textContent.trim());
    } catch (e) {
      document.body.innerHTML =
        "<p style='color:#eee;font:16px sans-serif;padding:2rem'>" +
        "This file carries no parade (" +
        e.message +
        ").</p>";
      return;
    }
    try {
      player = new Player(bytes);
    } catch (e) {
      document.body.innerHTML =
        "<p style='color:#eee;font:16px sans-serif;padding:2rem'>" +
        "This file carries no parade (" +
        e.message +
        ").</p>";
      return;
    }
    context = canvas.getContext("2d");
    canvas.width = player.head.cols * player.head.cellW;
    canvas.height = player.head.rows * player.head.cellH;
    buildSprites(player.head);
    scrub.max = String(Math.max(0, player.head.frameCount - 1));
    scrub.addEventListener("input", function () {
      playing = false;
      play.textContent = "play";
      showFrame(parseInt(scrub.value, 10) || 0);
    });
    play.addEventListener("click", function () {
      playing = !playing;
      play.textContent = playing ? "pause" : "play";
      lastTime = 0;
    });
    var name = manifest && manifest.name ? manifest.name : "a parade";
    var title = document.getElementById("title");
    if (title) {
      title.textContent = name;
    }
    var info = document.getElementById("info");
    if (info) {
      info.textContent =
        "made with AGnosis — " +
        player.head.frameCount +
        " frames at " +
        player.head.fps +
        " fps, " +
        player.head.cols +
        "x" +
        player.head.rows +
        " cells";
    }
    player.step();
    draw();
    requestAnimationFrame(loop);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
  } else {
    start();
  }
})();
