import * as THREE from './vendor/three/three.module.min.js';

const STYLE = `
  :host { display:block; position:absolute; inset:0; overflow:hidden; pointer-events:none; contain:layout paint; }
  .stage { position:absolute; inset:0; overflow:hidden; pointer-events:none; }
  canvas { position:absolute; inset:0; display:block; width:100%; height:100%; pointer-events:none; }
  .fallback { position:absolute; inset:0; overflow:hidden; pointer-events:none; background:linear-gradient(180deg,rgba(246,242,233,.16),rgba(236,224,204,.1)); }
  .fallback[hidden] { display:none; }
  .fallback-wall { position:absolute; inset:0 0 32%; background:linear-gradient(112deg,rgba(255,250,238,.64),rgba(239,233,220,.3)); }
  .fallback-floor { position:absolute; inset:62% -8% -12%; background:repeating-linear-gradient(96deg,transparent 0,transparent 70px,rgba(127,94,59,.07) 71px,transparent 72px),linear-gradient(180deg,#dfcdb2,#cbb18e); transform:perspective(520px) rotateX(48deg); transform-origin:50% 0; }
  .fallback-window { position:absolute; top:12%; left:8%; width:clamp(105px,17vw,230px); aspect-ratio:1.18; padding:7px; border:7px solid #eee5d5; background:linear-gradient(165deg,#c9dfe4,#eef1df 68%,#b9c8a7 69%); box-shadow:0 5px 18px rgba(91,74,52,.08),inset 0 0 0 1px rgba(108,129,125,.15); }
  .fallback-window::before,.fallback-window::after { content:""; position:absolute; background:#eee5d5; }
  .fallback-window::before { top:0; bottom:0; left:50%; width:5px; transform:translateX(-50%); }
  .fallback-window::after { top:50%; right:0; left:0; height:5px; transform:translateY(-50%); }
  .fallback-curtain { position:absolute; top:10%; left:6%; width:clamp(28px,4vw,54px); height:clamp(145px,25vw,270px); border-radius:8px 8px 18px 12px; background:repeating-linear-gradient(90deg,#e9dfcd 0,#f5eddf 10px,#dfd2bd 20px); opacity:.88; transform-origin:top; }
  .fallback-curtain.right { left:calc(8% + clamp(105px,17vw,230px) - 12px); transform:scaleX(-1); }
  .fallback-shelf { position:absolute; bottom:29%; left:3%; width:clamp(90px,12vw,165px); height:clamp(145px,23vw,245px); border:9px solid #a9855e; border-bottom-width:13px; background:repeating-linear-gradient(180deg,rgba(239,225,201,.8) 0,rgba(239,225,201,.8) 29%,#a9855e 30%,#a9855e 33%); box-shadow:0 9px 20px rgba(83,62,40,.1); }
  .fallback-books { position:absolute; right:12%; bottom:12%; left:12%; height:26%; background:linear-gradient(90deg,#879b89 0 16%,transparent 16% 20%,#c48262 20% 38%,transparent 38% 42%,#d6b568 42% 57%,transparent 57% 62%,#7895a2 62% 82%,transparent 82%); }
  .fallback-sofa { position:absolute; right:4%; bottom:27%; width:clamp(180px,28vw,380px); height:clamp(85px,13vw,158px); border-radius:28px 28px 15px 15px; background:#c8b69c; box-shadow:0 16px 25px rgba(86,67,46,.12),inset 0 2px 0 rgba(255,255,255,.35); }
  .fallback-sofa::before { content:""; position:absolute; right:7%; bottom:0; left:7%; height:53%; border-radius:17px; background:linear-gradient(90deg,#d8c9b4 0 48%,#b6a48d 48% 50%,#d8c9b4 50%); }
  .fallback-sofa::after { content:""; position:absolute; top:15%; right:14%; width:23%; aspect-ratio:1; border-radius:12px; background:#91a18a; transform:rotate(7deg); box-shadow:-95px 8px 0 -8px #cf9d70; }
  .fallback-rug { position:absolute; bottom:4%; left:50%; width:min(46vw,510px); height:23%; border:1px solid rgba(158,128,92,.17); border-radius:50%; background:repeating-linear-gradient(0deg,rgba(166,139,105,.07) 0,rgba(166,139,105,.07) 1px,transparent 1px,transparent 13px),#e9ddc9; box-shadow:0 10px 24px rgba(93,69,43,.07); transform:translateX(-50%); }
  .fallback-toys { position:absolute; bottom:20%; left:24%; width:38px; height:57px; border-radius:50% 50% 12px 12px; background:linear-gradient(180deg,#c99264 0 18%,#d6b568 18% 37%,#8fa18b 37% 58%,#7895a2 58% 80%,#b98262 80%); box-shadow:inset 0 0 0 5px rgba(255,255,255,.2); }
  .fallback-blocks { position:absolute; right:25%; bottom:14%; width:34px; height:34px; border-radius:5px; background:#c98e69; box-shadow:34px 8px 0 #d2b46f; transform:rotate(-8deg); }
  @media (max-width:760px) {
    .fallback-shelf { left:-30px; opacity:.65; }
    .fallback-sofa { right:-58px; opacity:.7; }
    .fallback-window { left:3%; opacity:.72; }
    .fallback-curtain { left:1%; opacity:.65; }
    .fallback-curtain.right { left:calc(3% + clamp(105px,17vw,230px) - 12px); }
    .fallback-rug { width:min(92vw,480px); }
    .fallback-toys { left:8%; }
    .fallback-blocks { right:9%; }
  }
  @media (prefers-reduced-motion:reduce) {
    *,*::before,*::after { animation:none!important; transition:none!important; }
  }
`;

function roundedBox(width, height, depth, radius = 0.08, bevel = 0.025) {
  const edge = Math.min(bevel, width / 4, height / 4, depth / 4);
  const w = width / 2 - edge;
  const h = height / 2 - edge;
  const r = Math.min(radius, w, h);
  const shape = new THREE.Shape();
  shape.moveTo(-w + r, -h);
  shape.lineTo(w - r, -h);
  shape.quadraticCurveTo(w, -h, w, -h + r);
  shape.lineTo(w, h - r);
  shape.quadraticCurveTo(w, h, w - r, h);
  shape.lineTo(-w + r, h);
  shape.quadraticCurveTo(-w, h, -w, h - r);
  shape.lineTo(-w, -h + r);
  shape.quadraticCurveTo(-w, -h, -w + r, -h);
  const geometry = new THREE.ExtrudeGeometry(shape, {
    depth: Math.max(0.001, depth - 2 * edge), bevelEnabled: edge > 0,
    bevelSegments: 3, steps: 1, bevelSize: edge, bevelThickness: edge,
    curveSegments: 6,
  });
  geometry.translate(0, 0, -depth / 2 + edge);
  geometry.computeVertexNormals();
  return geometry;
}

function roundedRect(context, x, y, width, height, radius) {
  const r = Math.min(radius, width / 2, height / 2);
  context.beginPath();
  context.moveTo(x + r, y);
  context.lineTo(x + width - r, y);
  context.quadraticCurveTo(x + width, y, x + width, y + r);
  context.lineTo(x + width, y + height - r);
  context.quadraticCurveTo(x + width, y + height, x + width - r, y + height);
  context.lineTo(x + r, y + height);
  context.quadraticCurveTo(x, y + height, x, y + height - r);
  context.lineTo(x, y + r);
  context.quadraticCurveTo(x, y, x + r, y);
  context.closePath();
}

class AiTutorRoom extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    this.shadowRoot.innerHTML = `<style>${STYLE}</style><div class="stage"><div class="fallback" aria-hidden="true"><div class="fallback-wall"></div><div class="fallback-floor"></div><div class="fallback-window"></div><div class="fallback-curtain"></div><div class="fallback-curtain right"></div><div class="fallback-shelf"><div class="fallback-books"></div></div><div class="fallback-sofa"></div><div class="fallback-rug"></div><div class="fallback-toys"></div><div class="fallback-blocks"></div></div></div>`;
    this._stage = this.shadowRoot.querySelector('.stage');
    this._fallback = this.shadowRoot.querySelector('.fallback');
    this._connected = false;
    this._contextLost = false;
    this._reducedMotion = false;
    this._inView = true;
    this._frame = 0;
    this._lastFrame = 0;
    this._elapsed = 0;
    this._listeners = [];
    this._geometries = new Set();
    this._materials = new Set();
    this._textures = new Set();
    this._curtains = [];
    this._renderer = null;
    this._scene = null;
    this._camera = null;
    this._boundTick = timestamp => this._tick(timestamp);
  }

  connectedCallback() {
    if (this._connected) return;
    this.setAttribute('aria-hidden', 'true');
    this._connected = true;
    this._contextLost = false;
    this._fallback.hidden = false;
    this.dataset.renderState = 'loading';
    delete this.dataset.fallbackReason;
    this._motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)');
    this._reducedMotion = this._motionQuery.matches;
    this._listen(this._motionQuery, 'change', event => {
      this._reducedMotion = event.matches;
      this._lastFrame = 0;
      if (this._reducedMotion) this._restCurtains();
      this._syncPlayback();
    });
    this._listen(document, 'visibilitychange', () => {
      this._lastFrame = 0;
      this._syncPlayback();
    });
    this._listen(window, 'resize', () => this._resize(), { passive: true });
    if ('ResizeObserver' in window) {
      this._resizeObserver = new ResizeObserver(() => this._resize());
      this._resizeObserver.observe(this);
    }
    if ('IntersectionObserver' in window) {
      this._intersectionObserver = new IntersectionObserver(entries => {
        this._inView = entries[0]?.isIntersecting ?? true;
        this._lastFrame = 0;
        this._syncPlayback();
      }, { threshold: 0 });
      this._intersectionObserver.observe(this);
    }
    try {
      this._createRenderer();
      this._buildRoom();
      this._resize();
      this._render();
      this._markReady();
    } catch {
      this._activateFallback('webgl-unavailable');
      this._disposeGraphics();
    }
    this._syncPlayback();
  }

  disconnectedCallback() {
    this._connected = false;
    this._stopAnimation();
    this._resizeObserver?.disconnect();
    this._intersectionObserver?.disconnect();
    this._resizeObserver = null;
    this._intersectionObserver = null;
    for (const remove of this._listeners.splice(0)) remove();
    this._motionQuery = null;
    this._disposeGraphics();
  }

  _listen(target, name, handler, options) {
    target.addEventListener(name, handler, options);
    this._listeners.push(() => target.removeEventListener(name, handler, options));
  }

  _createRenderer() {
    this._renderer = new THREE.WebGLRenderer({
      alpha: true, antialias: true, powerPreference: 'low-power', premultipliedAlpha: true,
    });
    this._renderer.setClearColor(0x000000, 0);
    this._renderer.outputColorSpace = THREE.SRGBColorSpace;
    this._renderer.toneMapping = THREE.ACESFilmicToneMapping;
    this._renderer.toneMappingExposure = 1.03;
    this._renderer.shadowMap.enabled = true;
    this._renderer.shadowMap.type = THREE.PCFSoftShadowMap;
    const canvas = this._renderer.domElement;
    canvas.className = 'room-canvas';
    canvas.setAttribute('aria-hidden', 'true');
    this._stage.prepend(canvas);
    this._listen(canvas, 'webglcontextlost', event => {
      event.preventDefault();
      if (!this._connected || this._contextLost) return;
      this._contextLost = true;
      this._stopAnimation();
      canvas.style.visibility = 'hidden';
      this._activateFallback('context-lost');
    });
    this._listen(canvas, 'webglcontextrestored', () => {
      if (!this._connected || !this._renderer) return;
      try {
        this._contextLost = false;
        canvas.style.visibility = '';
        this._resize();
        this._render();
        this._markReady();
        this._syncPlayback();
      } catch {
        this._activateFallback('context-restore-failed');
        this._disposeGraphics();
      }
    });
  }

  _geometry(geometry) {
    this._geometries.add(geometry);
    return geometry;
  }

  _material(material) {
    this._materials.add(material);
    return material;
  }

  _standard(color, options = {}) {
    return this._material(new THREE.MeshStandardMaterial({
      color, roughness: 0.82, metalness: 0, ...options,
    }));
  }

  _mesh(parent, geometry, material, x = 0, y = 0, z = 0) {
    const mesh = new THREE.Mesh(this._geometry(geometry), material);
    mesh.position.set(x, y, z);
    mesh.castShadow = true;
    mesh.receiveShadow = true;
    parent.add(mesh);
    return mesh;
  }

  _box(parent, width, height, depth, material, x, y, z, radius = 0) {
    return this._mesh(parent, radius > 0
      ? roundedBox(width, height, depth, radius)
      : new THREE.BoxGeometry(width, height, depth), material, x, y, z);
  }

  _canvasTexture(width, height, draw, repeat = false) {
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext('2d');
    if (!context) throw new Error('Canvas 2D is unavailable');
    draw(context, width, height);
    const texture = new THREE.CanvasTexture(canvas);
    texture.colorSpace = THREE.SRGBColorSpace;
    texture.anisotropy = Math.min(this._renderer.capabilities.getMaxAnisotropy(), 4);
    if (repeat) {
      texture.wrapS = THREE.RepeatWrapping;
      texture.wrapT = THREE.RepeatWrapping;
    }
    this._textures.add(texture);
    return texture;
  }

  _buildRoom() {
    this._scene = new THREE.Scene();
    this._scene.fog = new THREE.Fog(0xf2ede4, 17, 31);
    this._camera = new THREE.PerspectiveCamera(36, 1, 0.1, 60);
    this._buildLighting();
    this._buildArchitecture();
    this._buildWindow();
    this._buildBookcase();
    this._buildSofa();
    this._buildRug();
    this._buildToys();
  }

  _buildLighting() {
    this._scene.add(new THREE.HemisphereLight(0xf4f5ed, 0x8d755a, 1.35));
    this._scene.add(new THREE.AmbientLight(0xfff4e4, 0.42));
    const sunlight = new THREE.DirectionalLight(0xffdfad, 2.65);
    sunlight.position.set(-4.8, 7.5, 4.5);
    sunlight.castShadow = true;
    sunlight.shadow.mapSize.set(1024, 1024);
    Object.assign(sunlight.shadow.camera, {
      left: -7, right: 7, top: 7, bottom: -5, near: 0.5, far: 24,
    });
    sunlight.shadow.normalBias = 0.035;
    sunlight.shadow.bias = -0.00025;
    sunlight.shadow.radius = 3;
    this._scene.add(sunlight);
    const fill = new THREE.DirectionalLight(0xdce9ec, 0.8);
    fill.position.set(5, 3, 4);
    this._scene.add(fill);
  }

  _buildArchitecture() {
    const wall = this._standard(0xeee8dc, { roughness: 0.96 });
    const trim = this._standard(0xe4dac8, { roughness: 0.9 });
    const backWall = this._box(this._scene, 13, 5.8, 0.16, wall, 0, 2.9, -4);
    backWall.castShadow = false;
    const sideWall = this._box(this._scene, 0.16, 5.8, 6.2, wall, -6.42, 2.9, -0.95);
    sideWall.castShadow = false;
    this._box(this._scene, 12.85, 0.18, 0.13, trim, 0, 0.09, -3.86, 0.025);
    this._box(this._scene, 0.13, 0.18, 6, trim, -6.29, 0.09, -0.9, 0.025);

    const wood = this._canvasTexture(512, 512, (context, width, height) => {
      context.fillStyle = '#c7a77e';
      context.fillRect(0, 0, width, height);
      const tones = ['#c9aa81', '#c2a077', '#ccad84', '#c5a47b'];
      for (let row = 0; row < 8; row += 1) {
        const y = row * 64;
        context.fillStyle = tones[row % tones.length];
        context.fillRect(0, y + 1, width, 62);
        context.strokeStyle = 'rgba(104,73,43,.2)';
        context.lineWidth = 1;
        context.beginPath();
        context.moveTo(0, y + 63.5);
        context.lineTo(width, y + 63.5);
        context.stroke();
        const offset = row % 2 ? 168 : 350;
        context.beginPath();
        context.moveTo(offset, y);
        context.lineTo(offset, y + 64);
        context.stroke();
        for (let grain = 0; grain < 7; grain += 1) {
          const gy = y + 8 + grain * 7;
          context.strokeStyle = `rgba(111,77,43,${0.025 + (grain % 3) * 0.012})`;
          context.beginPath();
          for (let x = 0; x <= width; x += 16) {
            const wave = Math.sin(x * 0.035 + row * 1.7 + grain) * 1.8;
            if (x === 0) context.moveTo(x, gy + wave);
            else context.lineTo(x, gy + wave);
          }
          context.stroke();
        }
      }
    }, true);
    wood.repeat.set(2.8, 2.1);
    const floorMaterial = this._standard(0xffffff, { map: wood, roughness: 0.79 });
    const floor = this._mesh(this._scene, new THREE.PlaneGeometry(14, 10), floorMaterial, 0, -0.015, 0.7);
    floor.rotation.x = -Math.PI / 2;
    floor.castShadow = false;

    const sunlightMaterial = this._material(new THREE.MeshBasicMaterial({
      color: 0xffe5ae, transparent: true, opacity: 0.075, depthWrite: false,
      side: THREE.DoubleSide,
    }));
    const patch = this._mesh(this._scene, new THREE.PlaneGeometry(2.7, 3.8), sunlightMaterial, -2.25, 0.008, 0.15);
    patch.rotation.x = -Math.PI / 2;
    patch.rotation.z = -0.32;
    patch.castShadow = false;
    patch.receiveShadow = false;
  }

  _buildWindow() {
    const group = new THREE.Group();
    group.position.set(-3.65, 3.45, -3.89);
    this._scene.add(group);
    const sky = this._canvasTexture(256, 256, (context, width, height) => {
      const gradient = context.createLinearGradient(0, 0, 0, height);
      gradient.addColorStop(0, '#bfd8df');
      gradient.addColorStop(0.67, '#edf0dc');
      gradient.addColorStop(1, '#b8c7a4');
      context.fillStyle = gradient;
      context.fillRect(0, 0, width, height);
      context.fillStyle = 'rgba(250,246,218,.72)';
      context.beginPath();
      context.arc(55, 58, 23, 0, Math.PI * 2);
      context.fill();
      context.fillStyle = '#aabca0';
      context.beginPath();
      context.moveTo(0, 211);
      context.quadraticCurveTo(70, 154, 143, 211);
      context.quadraticCurveTo(205, 172, 256, 203);
      context.lineTo(256, 256);
      context.lineTo(0, 256);
      context.fill();
    });
    const skyMaterial = this._material(new THREE.MeshBasicMaterial({ map: sky }));
    const pane = this._mesh(group, new THREE.PlaneGeometry(2.42, 2.02), skyMaterial, 0, 0, 0.015);
    pane.castShadow = false;
    pane.receiveShadow = false;
    const frame = this._standard(0xeee4d2, { roughness: 0.75 });
    for (const x of [-1.28, 1.28]) this._box(group, 0.14, 2.3, 0.14, frame, x, 0, 0.09, 0.025);
    for (const y of [-1.08, 1.08]) this._box(group, 2.7, 0.14, 0.14, frame, 0, y, 0.09, 0.025);
    this._box(group, 0.075, 2.08, 0.09, frame, 0, 0, 0.1, 0.015);
    this._box(group, 2.48, 0.075, 0.09, frame, 0, 0, 0.1, 0.015);
    this._box(group, 2.9, 0.13, 0.38, frame, 0, -1.18, 0.16, 0.03);
    const rodMaterial = this._standard(0x947654, { roughness: 0.55 });
    const rod = this._mesh(group, new THREE.CylinderGeometry(0.035, 0.035, 3.35, 16), rodMaterial, 0, 1.32, 0.21);
    rod.rotation.z = Math.PI / 2;
    const curtainMaterial = this._standard(0xe6dbc7, { roughness: 1 });
    this._curtains = [];
    for (const side of [-1, 1]) {
      const pivot = new THREE.Group();
      pivot.position.set(side * 1.46, 1.18, 0.25);
      group.add(pivot);
      this._box(pivot, 0.5, 2.38, 0.1, curtainMaterial, 0, -1.08, 0, 0.09);
      for (const fold of [-0.14, 0, 0.14]) {
        const ridge = this._box(pivot, 0.035, 2.24, 0.035, frame, fold, -1.08, 0.065, 0.012);
        ridge.castShadow = false;
      }
      pivot.rotation.z = side * 0.015;
      this._curtains.push({ pivot, side });
    }
  }

  _buildBookcase() {
    const group = new THREE.Group();
    group.position.set(-4.78, 0, -3.38);
    this._scene.add(group);
    const wood = this._standard(0xa68159, { roughness: 0.72 });
    const back = this._standard(0xc7ab84, { roughness: 0.9 });
    this._box(group, 1.82, 2.5, 0.1, back, 0, 1.25, 0, 0.025);
    for (const x of [-0.87, 0.87]) this._box(group, 0.14, 2.58, 0.54, wood, x, 1.29, 0.22, 0.025);
    for (const y of [0.1, 0.83, 1.56, 2.31]) this._box(group, 1.82, 0.13, 0.57, wood, 0, y, 0.23, 0.025);
    const bookColors = [0x7f9685, 0xc17f5e, 0xd2b267, 0x76929f, 0xe0d2b8, 0xa86750];
    const books = [
      [-0.65, 0.46, 0.18, 0.55, 0], [-0.44, 0.43, 0.19, 0.49, 0],
      [-0.22, 0.45, 0.16, 0.53, 0.04], [0.32, 1.17, 0.2, 0.5, 0],
      [0.55, 1.18, 0.18, 0.52, -0.035], [-0.57, 1.91, 0.17, 0.52, 0],
      [-0.36, 1.88, 0.19, 0.47, 0], [0.18, 1.9, 0.18, 0.51, 0.025],
    ];
    books.forEach(([x, y, width, height, tilt], index) => {
      const material = this._standard(bookColors[index % bookColors.length], { roughness: 0.86 });
      const book = this._box(group, width, height, 0.34, material, x, y, 0.37, 0.015);
      book.rotation.z = tilt;
    });
    const basket = this._standard(0xbca17c, { roughness: 1 });
    this._box(group, 0.72, 0.38, 0.4, basket, 0.38, 0.35, 0.35, 0.07);
  }

  _buildSofa() {
    const group = new THREE.Group();
    group.position.set(3.35, 0, -2.55);
    group.rotation.y = -0.055;
    this._scene.add(group);
    const fabric = this._standard(0xc6b49b, { roughness: 1 });
    const cushions = this._standard(0xd7c8b2, { roughness: 1 });
    const wood = this._standard(0x8a6b4d, { roughness: 0.7 });
    this._box(group, 3.45, 0.42, 1.25, fabric, 0, 0.45, 0, 0.12);
    this._box(group, 3.35, 1.02, 0.34, fabric, 0, 1.02, -0.48, 0.13);
    for (const x of [-1.56, 1.56]) this._box(group, 0.34, 0.75, 1.28, fabric, x, 0.72, 0, 0.12);
    for (const x of [-0.72, 0.72]) this._box(group, 1.39, 0.22, 0.96, cushions, x, 0.76, 0.1, 0.09);
    for (const x of [-1.25, 1.25]) {
      for (const z of [-0.38, 0.38]) this._box(group, 0.13, 0.24, 0.13, wood, x, 0.12, z, 0.025);
    }
    const sage = this._standard(0x8f9f88, { roughness: 1 });
    const clay = this._standard(0xc58d68, { roughness: 1 });
    const pillowA = this._box(group, 0.66, 0.64, 0.2, sage, 0.91, 1.12, -0.2, 0.13);
    pillowA.rotation.z = -0.11;
    const pillowB = this._box(group, 0.56, 0.55, 0.19, clay, -0.98, 1.08, -0.19, 0.12);
    pillowB.rotation.z = 0.13;
  }

  _buildRug() {
    const texture = this._canvasTexture(512, 336, (context, width, height) => {
      context.clearRect(0, 0, width, height);
      roundedRect(context, 4, 4, width - 8, height - 8, 54);
      context.fillStyle = '#e6dac5';
      context.fill();
      context.strokeStyle = 'rgba(151,121,85,.25)';
      context.lineWidth = 3;
      roundedRect(context, 20, 20, width - 40, height - 40, 43);
      context.stroke();
      context.lineWidth = 1;
      for (let y = 35; y < height - 25; y += 13) {
        context.strokeStyle = y % 2 ? 'rgba(139,111,79,.055)' : 'rgba(255,255,255,.2)';
        context.beginPath();
        context.moveTo(42, y);
        context.lineTo(width - 42, y);
        context.stroke();
      }
    });
    const material = this._standard(0xffffff, {
      map: texture, transparent: true, alphaTest: 0.02, roughness: 1, depthWrite: false,
    });
    const rug = this._mesh(this._scene, new THREE.PlaneGeometry(4.9, 3.2), material, 0, 0.018, 0.85);
    rug.rotation.x = -Math.PI / 2;
    rug.castShadow = false;
    rug.renderOrder = 1;
  }

  _buildToys() {
    const rings = new THREE.Group();
    rings.position.set(-2.55, 0.025, 1.45);
    rings.rotation.y = 0.18;
    this._scene.add(rings);
    const wood = this._standard(0xb38b5f, { roughness: 0.7 });
    this._mesh(rings, new THREE.CylinderGeometry(0.38, 0.42, 0.1, 32), wood, 0, 0.05, 0);
    this._mesh(rings, new THREE.CylinderGeometry(0.04, 0.05, 0.75, 16), wood, 0, 0.43, 0);
    const colors = [0x7795a2, 0x8fa088, 0xd0ad61, 0xc38360];
    colors.forEach((color, index) => {
      const material = this._standard(color, { roughness: 0.65 });
      const ring = this._mesh(rings, new THREE.TorusGeometry(0.29 - index * 0.047, 0.063, 12, 32), material, 0, 0.17 + index * 0.125, 0);
      ring.rotation.x = Math.PI / 2;
    });
    this._mesh(rings, new THREE.SphereGeometry(0.105, 20, 14), this._standard(0xc38360), 0, 0.75, 0);

    const blockA = this._box(this._scene, 0.43, 0.43, 0.43, this._standard(0xc88d68), 2.45, 0.235, 1.42, 0.045);
    blockA.rotation.y = 0.2;
    const blockB = this._box(this._scene, 0.4, 0.4, 0.4, this._standard(0xd1b36d), 2.93, 0.215, 1.62, 0.045);
    blockB.rotation.y = -0.16;
    const ball = this._mesh(this._scene, new THREE.SphereGeometry(0.24, 24, 18), this._standard(0x83998a, { roughness: 0.72 }), 3.18, 0.24, 1.03);
    ball.rotation.z = 0.2;
  }

  _resize() {
    if (!this._connected || !this._renderer || !this._camera || this._contextLost) return;
    const width = Math.round(this.clientWidth);
    const height = Math.round(this.clientHeight);
    if (width < 1 || height < 1) return;
    const aspect = width / height;
    const narrow = aspect < 0.85;
    this._camera.aspect = aspect;
    this._camera.fov = narrow ? 45 : 36;
    this._camera.position.set(narrow ? 4.4 : 6.7, narrow ? 4.6 : 5.1, narrow ? 13.8 : 11.5);
    this._camera.lookAt(0, 1.35, -0.5);
    this._camera.updateProjectionMatrix();
    this._renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, width < 600 ? 1.5 : 1.75));
    this._renderer.setSize(width, height, false);
    this._lastFrame = 0;
    this._syncPlayback();
  }

  _restCurtains() {
    for (const { pivot, side } of this._curtains) pivot.rotation.z = side * 0.015;
  }

  _render() {
    if (!this._renderer || !this._scene || !this._camera || this._contextLost || document.hidden) return;
    this._renderer.render(this._scene, this._camera);
  }

  _renderSafely() {
    try {
      this._render();
    } catch {
      this._activateFallback('render-failed');
      this._disposeGraphics();
    }
  }

  _markReady() {
    this._fallback.hidden = true;
    delete this.dataset.fallbackReason;
    this.dataset.renderState = 'ready';
    this.dispatchEvent(new CustomEvent('room-ready', {
      bubbles: true, composed: true, detail: { renderer: 'three-webgl', version: THREE.REVISION },
    }));
  }

  _activateFallback(reason) {
    this._stopAnimation();
    this._fallback.hidden = false;
    this.dataset.renderState = 'fallback';
    this.dataset.fallbackReason = reason;
    this.dispatchEvent(new CustomEvent('room-error', {
      bubbles: true, composed: true, detail: { reason, fallback: 'local-css' },
    }));
  }

  _shouldAnimate() {
    return this._connected && !this._reducedMotion && !document.hidden && this._inView
      && Boolean(this._renderer) && !this._contextLost;
  }

  _syncPlayback() {
    this._stopAnimation();
    if (!this._connected || document.hidden || !this._inView || this._contextLost || !this._renderer) return;
    if (this._reducedMotion) {
      this._renderSafely();
      return;
    }
    this._frame = requestAnimationFrame(this._boundTick);
  }

  _stopAnimation() {
    cancelAnimationFrame(this._frame);
    this._frame = 0;
  }

  _tick(timestamp) {
    this._frame = 0;
    if (!this._shouldAnimate()) return;
    if (this._lastFrame && timestamp - this._lastFrame < 32) {
      this._frame = requestAnimationFrame(this._boundTick);
      return;
    }
    const delta = this._lastFrame ? Math.min((timestamp - this._lastFrame) / 1000, 0.1) : 0;
    this._lastFrame = timestamp;
    this._elapsed += delta;
    for (const { pivot, side } of this._curtains) {
      pivot.rotation.z = side * (0.015 + Math.sin(this._elapsed * 0.42) * 0.012);
    }
    try {
      this._renderer.render(this._scene, this._camera);
    } catch {
      this._activateFallback('render-failed');
      this._disposeGraphics();
      return;
    }
    this._frame = requestAnimationFrame(this._boundTick);
  }

  _disposeGraphics() {
    this._stopAnimation();
    this._contextLost = true;
    this._scene?.traverse(object => object.shadow?.dispose());
    for (const material of this._materials) material.dispose();
    for (const texture of this._textures) texture.dispose();
    for (const geometry of this._geometries) geometry.dispose();
    this._materials.clear();
    this._textures.clear();
    this._geometries.clear();
    if (this._renderer) {
      const canvas = this._renderer.domElement;
      this._renderer.dispose();
      this._renderer.forceContextLoss();
      canvas.remove();
    }
    this._scene?.clear();
    this._renderer = null;
    this._scene = null;
    this._camera = null;
    this._curtains = [];
    this._lastFrame = 0;
    this._elapsed = 0;
  }
}

if (!customElements.get('ai-tutor-room')) customElements.define('ai-tutor-room', AiTutorRoom);

export { AiTutorRoom };
