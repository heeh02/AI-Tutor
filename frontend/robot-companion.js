/**
 * Local, data-driven WebGL companion. Three.js r180 / MIT.
 *
 * <ai-tutor-robot pose='{ "pan_deg":90, "tilt_deg":90,
 *   "neutral_pan_deg":90, "neutral_tilt_deg":90,
 *   "source":"feedback", "connected":true, "fresh":true, "revision":1 }'
 *   mood="idle"></ai-tutor-robot>
 *
 * Accepted sources: feedback, commanded, demo. Unknown, disconnected or stale
 * samples hold the last accepted live head pose. Leaving demo restores the last
 * live pose (or neutral). camera_session_id isolates devices/capture sessions:
 * changing it clears the old live pose, including while demonstrating. Demo is
 * accepted only when supplied explicitly by the parent; this module never
 * invents PTZ samples or calls APIs.
 * Positive pan turns the robot to its left (screen right in the frontal view).
 * Positive tilt points up. Optional pan_sign/tilt_sign are +1 or -1 and default
 * to +1. Both angles are relative to the supplied neutral angles.
 * Drag/arrow keys change only the viewer camera. Double-click/Home reset it.
 * Public methods: resetView(), getPoseSnapshot(). Events: ready, error.
 */
import * as THREE from './vendor/three/three.module.min.js';

const DEG = Math.PI / 180;
const SOURCES = new Set(['feedback', 'commanded', 'demo']);
const MOODS = new Set(['idle', 'listening', 'thinking', 'speaking']);
const A11Y_COPY = Object.freeze({
  zh: {
    label: '奶白色 3D 机器人学习伙伴',
    help: '可拖动或用方向键观察机器人，双击或按 Home 键恢复视角。视角操作不会移动摄像头。机器人头部姿态由页面提供的数据驱动。',
  },
  en: {
    label: 'Cream-colored 3D robot learning companion',
    help: 'Drag or use the arrow keys to view the robot. Double-click or press Home to reset the view. View controls do not move the physical camera. The robot head follows pose data supplied by the page.',
  },
});
const DEFAULT_VIEW = Object.freeze({ azimuth: 0.22, elevation: 0.11 });
const PALETTE = Object.freeze({
  shell: 0xfff7e9, lavender: 0xc6bce3, dark: 0x242638,
  coral: 0xf58d78, eye: 0xa9f1ed, violet: 0xbdb0fa, joint: 0xa8a2b8,
});

const STYLE = `
  :host { display:block; position:relative; width:100%; height:100%; min-height:var(--robot-min-height,0px); contain:layout paint; }
  .stage { position:absolute; inset:0; overflow:hidden; border-radius:inherit; }
  canvas { display:block; width:100%; height:100%; outline:none; cursor:grab; touch-action:pan-y; -webkit-tap-highlight-color:transparent; }
  canvas.dragging { cursor:grabbing; }
  canvas:focus-visible { outline:2px solid #a89bd0; outline-offset:-5px; border-radius:18px; }
  .fallback { position:absolute; inset:0; display:grid; place-content:center; gap:12px; padding:32px; text-align:center; color:#797386; font:14px/1.7 system-ui,sans-serif; }
  .fallback[hidden] { display:none; }
  .fallback strong { display:block; color:#4a4559; font-size:17px; font-weight:600; }
  .fallback-icon { width:56px; height:56px; margin:0 auto; display:grid; place-items:center; border-radius:18px; background:#eee9f8; font-size:26px; }
  .sr-only { position:absolute; width:1px; height:1px; overflow:hidden; clip-path:inset(50%); white-space:nowrap; }
`;

function roundedBox(width, height, depth, radius = 0.2, bevel = 0.04) {
  const w = width / 2 - bevel;
  const h = height / 2 - bevel;
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
    depth: Math.max(0.001, depth - 2 * bevel), bevelEnabled: bevel > 0,
    bevelSegments: 5, steps: 1, bevelSize: bevel, bevelThickness: bevel,
    curveSegments: 10,
  });
  geometry.translate(0, 0, -depth / 2 + bevel);
  geometry.computeVertexNormals();
  return geometry;
}

function roundedMaterial(color, overrides = {}) {
  return new THREE.MeshPhysicalMaterial({
    color, roughness: 0.32, metalness: 0.04, clearcoat: 0.42,
    clearcoatRoughness: 0.24, ...overrides,
  });
}

function addMesh(parent, geometry, material, x = 0, y = 0, z = 0) {
  const mesh = new THREE.Mesh(geometry, material);
  mesh.position.set(x, y, z);
  mesh.castShadow = true;
  mesh.receiveShadow = true;
  parent.add(mesh);
  return mesh;
}

function makeSoftTexture(color, stops) {
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = 128;
  const context = canvas.getContext('2d');
  const gradient = context.createRadialGradient(64, 64, 0, 64, 64, 64);
  for (const [offset, alpha] of stops) gradient.addColorStop(offset, `rgba(${color},${alpha})`);
  context.fillStyle = gradient;
  context.fillRect(0, 0, 128, 128);
  const texture = new THREE.CanvasTexture(canvas);
  texture.colorSpace = THREE.SRGBColorSpace;
  return texture;
}

function makeEnvironment(renderer) {
  // A tiny procedural studio. No image/model requests, even on first render.
  const scene = new THREE.Scene();
  scene.background = new THREE.Color(0xd9d4e0);
  const shell = new THREE.Mesh(new THREE.BoxGeometry(12, 12, 12),
    new THREE.MeshBasicMaterial({ color: 0x817988, side: THREE.BackSide }));
  scene.add(shell);
  for (const [x, y, z, width, height, color, intensity] of [
    [-3, 3, 3, 3.5, 5, 0xfff5e7, 5], [4, 1, 0, 2, 5, 0xe8ddff, 3],
    [0, 4, -3, 5, 2, 0xffffff, 4],
  ]) {
    const mat = new THREE.MeshBasicMaterial({ color, side: THREE.DoubleSide });
    mat.color.multiplyScalar(intensity);
    const softbox = new THREE.Mesh(new THREE.PlaneGeometry(width, height), mat);
    softbox.position.set(x, y, z);
    softbox.lookAt(0, 0, 0);
    scene.add(softbox);
  }
  const generator = new THREE.PMREMGenerator(renderer);
  const target = generator.fromScene(scene, 0.12, 0.1, 30, { size: 128 });
  generator.dispose();
  scene.traverse(object => { object.geometry?.dispose(); object.material?.dispose(); });
  return target;
}

class AiTutorRobot extends HTMLElement {
  static get observedAttributes() { return ['pose', 'mood', 'lang']; }

  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    this.shadowRoot.innerHTML = `<style>${STYLE}</style><div class="stage"><div class="fallback" role="status"><span class="fallback-icon" aria-hidden="true">✦</span><strong>正在唤醒小伙伴</strong><span>准备 3D 场景…</span></div></div><span id="robot-help" class="sr-only">可拖动或用左右方向键观察机器人，Home 键恢复视角。视角操作不会移动摄像头。机器人头部姿态由页面提供的数据驱动。</span>`;
    this._stage = this.shadowRoot.querySelector('.stage');
    this._fallback = this.shadowRoot.querySelector('.fallback');
    this._pose = { pan_deg: 90, tilt_deg: 90, neutral_pan_deg: 90, neutral_tilt_deg: 90,
      source: 'unknown', connected: false, fresh: false, revision: null };
    this._incoming = { ...this._pose };
    this._lastLivePose = null;
    this._cameraSessionId = null;
    this._headDegrees = { yaw: 0, pitch: 0 };
    this._mood = 'idle';
    this._view = { ...DEFAULT_VIEW };
    this._visible = true;
    this._frame = 0;
    this._listeners = [];
    this._lastFrame = 0;
    this._elapsed = 0;
    this._blinkAt = 3.8;
    this._disposed = true;
    this._contextLost = false;
    this._boundTick = timestamp => this._tick(timestamp);
  }

  connectedCallback() {
    if (!this._disposed) return;
    this._disposed = false;
    this._contextLost = false;
    this._motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)');
    this._reducedMotion = this._motionQuery.matches;
    this._listen(this._motionQuery, 'change', () => {
      this._reducedMotion = this._motionQuery.matches;
      this._animateDetails(0);
      this._requestFrame();
    });
    this._listen(document, 'visibilitychange', () => this._updateVisibility());
    try {
      this._buildScene();
      this._readLanguage();
      this._readPose(this.getAttribute('pose'));
      this._readMood(this.getAttribute('mood'));
      this._bindInteraction();
      this._resizeObserver = new ResizeObserver(() => this._resize());
      this._resizeObserver.observe(this);
      this._intersectionObserver = new IntersectionObserver(entries => {
        this._visible = entries[0]?.isIntersecting ?? true;
        this._updateVisibility();
      }, { threshold: 0 });
      this._intersectionObserver.observe(this);
      this._resize();
      this._animateDetails(0);
      this._renderer.render(this._scene, this._camera);
      this._fallback.hidden = true;
      this.dataset.renderState = 'ready';
      this.dispatchEvent(new CustomEvent('ready', { bubbles: true, composed: true,
        detail: { renderer: 'three-webgl', version: THREE.REVISION, pose: this.getPoseSnapshot() } }));
      this._requestFrame();
    } catch (error) {
      this._showError('webgl-unavailable', error);
      this.disconnectedCallback();
    }
  }

  disconnectedCallback() {
    this._disposed = true;
    cancelAnimationFrame(this._frame);
    this._frame = 0;
    this._resizeObserver?.disconnect();
    this._intersectionObserver?.disconnect();
    for (const remove of this._listeners.splice(0)) remove();
    this._resizeObserver = null;
    this._intersectionObserver = null;
    this._motionQuery = null;
    this._disposeScene();
  }

  attributeChangedCallback(name, oldValue, newValue) {
    if (oldValue === newValue) return;
    if (name === 'pose') this._readPose(newValue);
    if (name === 'mood') this._readMood(newValue);
    if (name === 'lang') this._readLanguage();
  }

  resetView() {
    this._view = { ...DEFAULT_VIEW };
    this._updateCamera();
    this._requestFrame();
  }

  getPoseSnapshot() {
    return {
      accepted: { ...this._pose }, incoming: { ...this._incoming },
      rendered: { yaw_deg: this._headDegrees.yaw, pitch_deg: this._headDegrees.pitch },
      euler: { y: this._yawPivot?.rotation.y ?? this._headDegrees.yaw * DEG,
        x: this._pitchPivot?.rotation.x ?? -this._headDegrees.pitch * DEG, order: 'yaw-then-local-pitch' },
      view: { azimuth_deg: this._view.azimuth / DEG, elevation_deg: this._view.elevation / DEG },
      mood: this._mood, renderer: this.dataset.renderState ?? 'pending',
    };
  }

  _listen(target, name, handler, options) {
    target.addEventListener(name, handler, options);
    this._listeners.push(() => target.removeEventListener(name, handler, options));
  }

  _readPose(value) {
    if (!value) return;
    let pose;
    try { pose = JSON.parse(value); }
    catch { this._poseError('Pose must be valid JSON.'); return; }
    if (!pose || typeof pose !== 'object' || Array.isArray(pose)) {
      this._poseError('Pose must be an object.'); return;
    }
    this._incoming = { ...pose };
    const sessionId = pose.camera_session_id ?? null;
    if (sessionId !== this._cameraSessionId) {
      this._cameraSessionId = sessionId;
      this._lastLivePose = null;
      this._acceptPose(this._neutralPose(pose));
    }
    if (this._pose.source === 'demo' && pose.source !== 'demo') {
      // Demonstration angles must never become the held hardware pose.
      this._acceptPose(this._lastLivePose ?? this._neutralPose(pose));
    }
    const valid = SOURCES.has(pose.source)
      && (pose.source === 'demo' ? pose.fresh !== false : pose.fresh === true && pose.connected === true)
      && Number.isFinite(pose.pan_deg) && Number.isFinite(pose.tilt_deg)
      && (pose.neutral_pan_deg === undefined || Number.isFinite(pose.neutral_pan_deg))
      && (pose.neutral_tilt_deg === undefined || Number.isFinite(pose.neutral_tilt_deg))
      && (pose.pan_sign === undefined || pose.pan_sign === 1 || pose.pan_sign === -1)
      && (pose.tilt_sign === undefined || pose.tilt_sign === 1 || pose.tilt_sign === -1);
    this.dataset.poseSource = String(pose.source ?? 'unknown');
    this.dataset.poseHeld = String(!valid);
    if (!valid) return;
    this._acceptPose(pose);
    if (pose.source !== 'demo') this._lastLivePose = { ...this._pose };
  }

  _neutralPose(pose) {
    const pan = Number.isFinite(pose.neutral_pan_deg) ? pose.neutral_pan_deg : 90;
    const tilt = Number.isFinite(pose.neutral_tilt_deg) ? pose.neutral_tilt_deg : 90;
    return { pan_deg: pan, tilt_deg: tilt, neutral_pan_deg: pan, neutral_tilt_deg: tilt,
      source: 'unknown', connected: false, fresh: false, revision: null,
      camera_session_id: this._cameraSessionId };
  }

  _acceptPose(pose) {
    this._pose = { ...pose, neutral_pan_deg: pose.neutral_pan_deg ?? 90,
      neutral_tilt_deg: pose.neutral_tilt_deg ?? 90,
      pan_sign: pose.pan_sign ?? 1, tilt_sign: pose.tilt_sign ?? 1 };
    this._headDegrees = {
      yaw: (this._pose.pan_deg - this._pose.neutral_pan_deg) * this._pose.pan_sign,
      pitch: (this._pose.tilt_deg - this._pose.neutral_tilt_deg) * this._pose.tilt_sign,
    };
    // Exactly one data path changes head yaw/pitch. No idle/head animation.
    this._applyHeadPose();
    this._requestFrame();
  }

  _poseError(message) {
    this.dispatchEvent(new CustomEvent('error', { bubbles: true, composed: true,
      detail: { code: 'invalid-pose', message, fatal: false } }));
  }

  _applyHeadPose() {
    if (!this._yawPivot) return;
    this._yawPivot.rotation.y = this._headDegrees.yaw * DEG;
    this._pitchPivot.rotation.x = -this._headDegrees.pitch * DEG;
  }

  _readMood(value) {
    this._mood = MOODS.has(value) ? value : 'idle';
    if (this._eyeMaterial) {
      this._eyeMaterial.color.setHex(this._mood === 'thinking' ? PALETTE.violet : PALETTE.eye);
      this._chestMaterial.color.setHex(this._mood === 'speaking' ? PALETTE.coral : PALETTE.lavender);
      this._chestMaterial.emissive.copy(this._chestMaterial.color);
    }
    this._requestFrame();
  }

  _readLanguage() {
    const copy = A11Y_COPY[this.getAttribute('lang') === 'en' ? 'en' : 'zh'];
    this.shadowRoot.querySelector('#robot-help').textContent = copy.help;
    this._renderer?.domElement.setAttribute('aria-label', copy.label);
  }

  _buildScene() {
    this._scene = new THREE.Scene();
    this._camera = new THREE.OrthographicCamera(-2.5, 2.5, 2.2, -2.2, 0.1, 30);
    this._renderer = new THREE.WebGLRenderer({ alpha: true, antialias: true, powerPreference: 'low-power' });
    this._renderer.setClearColor(0x000000, 0);
    this._renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.75));
    this._renderer.outputColorSpace = THREE.SRGBColorSpace;
    this._renderer.toneMapping = THREE.ACESFilmicToneMapping;
    this._renderer.toneMappingExposure = 1.18;
    this._renderer.shadowMap.enabled = true;
    this._renderer.shadowMap.type = THREE.PCFSoftShadowMap;
    const canvas = this._renderer.domElement;
    canvas.tabIndex = 0;
    canvas.setAttribute('role', 'img');
    canvas.setAttribute('aria-label', A11Y_COPY[this.getAttribute('lang') === 'en' ? 'en' : 'zh'].label);
    canvas.setAttribute('aria-describedby', 'robot-help');
    this._stage.prepend(canvas);
    this._environment = makeEnvironment(this._renderer);
    this._scene.environment = this._environment.texture;
    this._scene.environmentIntensity = 0.42;

    this._scene.add(new THREE.HemisphereLight(0xfffbf2, 0xb9a7c8, 2));
    const key = new THREE.DirectionalLight(0xfff1df, 3.6);
    key.position.set(-3, 5, 4);
    key.castShadow = true;
    key.shadow.mapSize.set(1024, 1024);
    Object.assign(key.shadow.camera, { left: -3, right: 3, top: 5, bottom: -2, near: 0.1, far: 15 });
    key.shadow.normalBias = 0.035;
    key.shadow.bias = -0.0002;
    key.shadow.radius = 3;
    this._scene.add(key);
    const fill = new THREE.DirectionalLight(0xd9d0ff, 2.2);
    fill.position.set(4, 2, 2);
    this._scene.add(fill);
    const rim = new THREE.DirectionalLight(0xffffff, 3);
    rim.position.set(1, 4, -4);
    this._scene.add(rim);

    const shell = roundedMaterial(PALETTE.shell);
    const lavender = roundedMaterial(PALETTE.lavender, { roughness: 0.43 });
    const joint = roundedMaterial(PALETTE.joint, { metalness: 0.45, roughness: 0.35 });
    const coral = roundedMaterial(PALETTE.coral, { roughness: 0.42 });
    const dark = roundedMaterial(PALETTE.dark, { roughness: 0.24, clearcoat: 0.9, metalness: 0.14 });
    const robot = new THREE.Group();
    this._scene.add(robot);

    // Stable lower body: broad soft shoes, rounded torso, short resting arms.
    for (const side of [-1, 1]) {
      addMesh(robot, roundedBox(0.47, 0.24, 0.63, 0.14, 0.055), lavender, side * 0.34, 0.18, 0.09);
      addMesh(robot, roundedBox(0.4, 0.24, 0.59, 0.13, 0.05), shell, side * 0.34, 0.26, 0.11);
      addMesh(robot, new THREE.CylinderGeometry(0.13, 0.14, 0.24, 24), joint, side * 0.32, 0.46, 0);
    }
    this._torso = addMesh(robot, roundedBox(1.1, 1.14, 0.81, 0.28, 0.1), shell, 0, 1.09, 0);
    addMesh(robot, roundedBox(0.7, 0.58, 0.07, 0.18, 0.025), lavender, 0, 1.11, 0.42);
    addMesh(robot, roundedBox(0.6, 0.49, 0.055, 0.15, 0.02), shell, 0, 1.12, 0.46);
    this._chestMaterial = roundedMaterial(PALETTE.lavender, { emissive: PALETTE.lavender, emissiveIntensity: 0.15 });
    const heart = new THREE.Shape();
    heart.moveTo(0, -0.095);
    heart.bezierCurveTo(-0.18, 0.025, -0.105, 0.16, 0, 0.075);
    heart.bezierCurveTo(0.105, 0.16, 0.18, 0.025, 0, -0.095);
    addMesh(robot, new THREE.ExtrudeGeometry(heart, { depth: 0.018, bevelEnabled: true,
      bevelThickness: 0.008, bevelSize: 0.008, bevelSegments: 3, steps: 1, curveSegments: 16 }),
    this._chestMaterial, 0, 1.14, 0.496);
    for (const side of [-1, 1]) {
      addMesh(robot, new THREE.SphereGeometry(0.16, 24, 16), lavender, side * 0.59, 1.35, 0);
      const arm = addMesh(robot, new THREE.CapsuleGeometry(0.145, 0.38, 8, 24), shell, side * 0.7, 1.05, 0.015);
      arm.rotation.z = side * 0.18;
      const hand = addMesh(robot, new THREE.SphereGeometry(0.16, 24, 16), lavender, side * 0.75, 0.78, 0.035);
      hand.scale.set(0.95, 1.03, 0.9);
      addMesh(robot, new THREE.SphereGeometry(0.085, 18, 12), shell, side * 0.66, 0.8, 0.12);
    }
    addMesh(robot, new THREE.CylinderGeometry(0.22, 0.25, 0.2, 40), joint, 0, 1.73, 0);
    addMesh(robot, new THREE.TorusGeometry(0.235, 0.035, 12, 40), lavender, 0, 1.73, 0).rotation.x = Math.PI / 2;

    // Yaw and pitch use separate pivots, matching a two-axis PTZ head.
    this._yawPivot = new THREE.Group();
    this._yawPivot.position.y = 1.81;
    robot.add(this._yawPivot);
    this._pitchPivot = new THREE.Group();
    this._yawPivot.add(this._pitchPivot);
    const head = this._pitchPivot;
    addMesh(head, roundedBox(1.91, 1.38, 1.2, 0.31, 0.14), shell, 0, 0.52, 0);
    addMesh(head, roundedBox(1.64, 1.03, 0.15, 0.27, 0.055), lavender, 0, 0.51, 0.564);
    addMesh(head, roundedBox(1.53, 0.93, 0.115, 0.25, 0.045), dark, 0, 0.515, 0.63);

    // Quiet little side discs and an actual lens on the crown.
    for (const side of [-1, 1]) {
      const ear = addMesh(head, new THREE.CylinderGeometry(0.23, 0.23, 0.11, 40), lavender, side * 0.98, 0.5, -0.03);
      ear.rotation.z = Math.PI / 2;
      const cap = addMesh(head, new THREE.CylinderGeometry(0.16, 0.16, 0.12, 32), shell, side * 1.02, 0.5, -0.03);
      cap.rotation.z = Math.PI / 2;
      const dot = addMesh(head, new THREE.CylinderGeometry(0.045, 0.045, 0.125, 20), coral, side * 1.04, 0.5, -0.03);
      dot.rotation.z = Math.PI / 2;
    }
    addMesh(head, new THREE.CylinderGeometry(0.055, 0.075, 0.22, 24), lavender, 0, 1.29, -0.13);
    this._antennaMaterial = roundedMaterial(PALETTE.coral, { emissive: PALETTE.coral, emissiveIntensity: 0.12 });
    addMesh(head, new THREE.SphereGeometry(0.11, 24, 16), this._antennaMaterial, 0, 1.43, -0.13);
    const lensBase = addMesh(head, new THREE.CylinderGeometry(0.075, 0.075, 0.025, 28), lavender, 0, 1.08, 0.594);
    lensBase.rotation.x = Math.PI / 2;
    const lens = addMesh(head, new THREE.CylinderGeometry(0.044, 0.044, 0.029, 28), dark, 0, 1.08, 0.615);
    lens.rotation.x = Math.PI / 2;
    addMesh(head, new THREE.SphereGeometry(0.014, 16, 10), new THREE.MeshBasicMaterial({ color: 0xcad7ff }), -0.012, 1.094, 0.637);

    this._eyeMaterial = new THREE.MeshBasicMaterial({ color: PALETTE.eye, toneMapped: false });
    const eyeRim = new THREE.MeshBasicMaterial({ color: 0x9ba5e8, toneMapped: false });
    const eyeHighlight = new THREE.MeshBasicMaterial({ color: 0xf1fffc, toneMapped: false });
    this._eyes = [];
    const glowTexture = makeSoftTexture('138,202,246', [[0, 0.22], [0.4, 0.12], [1, 0]]);
    for (const side of [-1, 1]) {
      const eye = new THREE.Group();
      eye.position.set(side * 0.32, 0.57, 0.7);
      head.add(eye);
      const glow = addMesh(eye, new THREE.PlaneGeometry(0.57, 0.65), new THREE.MeshBasicMaterial({
        map: glowTexture, transparent: true, depthWrite: false, toneMapped: false,
      }), 0, 0, -0.002);
      glow.castShadow = false;
      addMesh(eye, roundedBox(0.225, 0.34, 0.017, 0.1, 0.006), eyeRim);
      addMesh(eye, roundedBox(0.164, 0.28, 0.018, 0.077, 0.006), this._eyeMaterial, 0.008, 0.006, 0.011);
      addMesh(eye, new THREE.SphereGeometry(0.027, 16, 12), eyeHighlight, -0.035, 0.092, 0.027).scale.z = 0.2;
      this._eyes.push(eye);
    }
    const smileCurve = new THREE.QuadraticBezierCurve3(
      new THREE.Vector3(-0.12, 0.335, 0.709), new THREE.Vector3(0, 0.245, 0.715),
      new THREE.Vector3(0.12, 0.335, 0.709));
    this._smile = addMesh(head, new THREE.TubeGeometry(smileCurve, 24, 0.014, 8, false),
      new THREE.MeshBasicMaterial({ color: 0xb6cbdc, toneMapped: false }));
    for (const side of [-1, 1]) {
      addMesh(head, roundedBox(0.14, 0.038, 0.008, 0.016, 0.002),
        new THREE.MeshBasicMaterial({ color: 0xbc7d8d, transparent: true, opacity: 0.5 }), side * 0.56, 0.36, 0.692);
    }
    // A soft reflected strip makes the face read as a curved glass screen.
    const reflection = new THREE.QuadraticBezierCurve3(
      new THREE.Vector3(-0.53, 0.86, 0.689), new THREE.Vector3(-0.25, 0.92, 0.699),
      new THREE.Vector3(0.12, 0.89, 0.693));
    addMesh(head, new THREE.TubeGeometry(reflection, 24, 0.012, 6, false),
      new THREE.MeshBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0.07, depthWrite: false }));

    // Real shadow receiver plus a soft ambient contact shadow, on a transparent stage.
    const floor = addMesh(this._scene, new THREE.PlaneGeometry(30, 30),
      new THREE.ShadowMaterial({ color: 0x71627f, opacity: 0.16 }), 0, 0.055, 0);
    floor.rotation.x = -Math.PI / 2;
    floor.castShadow = false;
    const shadow = addMesh(this._scene, new THREE.PlaneGeometry(3.4, 2.65), new THREE.MeshBasicMaterial({
      map: makeSoftTexture('94,75,110', [[0, 0.19], [0.32, 0.12], [0.65, 0.03], [1, 0]]),
      transparent: true, depthWrite: false, toneMapped: false,
    }), 0, 0.058, 0);
    shadow.rotation.x = -Math.PI / 2;
    shadow.castShadow = false;
    shadow.receiveShadow = false;
    this._applyHeadPose();
    this._updateCamera();
  }

  _bindInteraction() {
    const canvas = this._renderer.domElement;
    this._listen(canvas, 'pointerdown', event => {
      if (!event.isPrimary || event.button !== 0) return;
      this._pointer = { id: event.pointerId, x: event.clientX, y: event.clientY };
      canvas.setPointerCapture(event.pointerId);
      canvas.classList.add('dragging');
    });
    this._listen(canvas, 'pointermove', event => {
      if (this._pointer?.id !== event.pointerId) return;
      const dx = event.clientX - this._pointer.x;
      const dy = event.clientY - this._pointer.y;
      this._pointer.x = event.clientX;
      this._pointer.y = event.clientY;
      this._view.azimuth = THREE.MathUtils.clamp(this._view.azimuth - dx * 0.006, -1.2, 1.2);
      this._view.elevation = THREE.MathUtils.clamp(this._view.elevation + dy * 0.004, -0.06, 0.52);
      this._updateCamera();
      this._requestFrame();
    });
    const endDrag = () => { this._pointer = null; canvas.classList.remove('dragging'); };
    this._listen(canvas, 'pointerup', endDrag);
    this._listen(canvas, 'pointercancel', endDrag);
    this._listen(canvas, 'lostpointercapture', endDrag);
    this._listen(canvas, 'dblclick', () => this.resetView());
    this._listen(canvas, 'keydown', event => {
      if (event.key === 'Home' || event.key === '0') { event.preventDefault(); this.resetView(); return; }
      const direction = { ArrowLeft: [-0.12, 0], ArrowRight: [0.12, 0], ArrowUp: [0, 0.06], ArrowDown: [0, -0.06] }[event.key];
      if (!direction) return;
      event.preventDefault();
      this._view.azimuth = THREE.MathUtils.clamp(this._view.azimuth + direction[0], -1.2, 1.2);
      this._view.elevation = THREE.MathUtils.clamp(this._view.elevation + direction[1], -0.06, 0.52);
      this._updateCamera();
      this._requestFrame();
    });
    this._listen(canvas, 'webglcontextlost', event => {
      event.preventDefault();
      this._contextLost = true;
      cancelAnimationFrame(this._frame);
      this._frame = 0;
      this._showError('webgl-context-lost', new Error('The graphics context was lost.'));
    });
    this._listen(canvas, 'webglcontextrestored', () => {
      try {
        this._environment?.dispose();
        this._environment = makeEnvironment(this._renderer);
        this._scene.environment = this._environment.texture;
        this._contextLost = false;
        this._fallback.hidden = true;
        this.dataset.renderState = 'ready';
        this._requestFrame();
        this.dispatchEvent(new CustomEvent('ready', { bubbles: true, composed: true,
          detail: { renderer: 'three-webgl', version: THREE.REVISION, restored: true } }));
      } catch (error) { this._showError('webgl-restore-failed', error); }
    });
  }

  _updateCamera() {
    if (!this._camera) return;
    const radius = 7;
    const { azimuth, elevation } = this._view;
    this._camera.position.set(radius * Math.sin(azimuth) * Math.cos(elevation),
      1.68 + radius * Math.sin(elevation), radius * Math.cos(azimuth) * Math.cos(elevation));
    this._camera.lookAt(0, 1.68, 0);
  }

  _resize() {
    if (!this._renderer || this._disposed) return;
    const { width, height } = this.getBoundingClientRect();
    if (width <= 0 || height <= 0) return;
    this._renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.75));
    this._renderer.setSize(width, height, false);
    const aspect = width / height;
    const viewHeight = Math.max(3.95, 2.8 / aspect);
    this._camera.left = -viewHeight * aspect / 2;
    this._camera.right = viewHeight * aspect / 2;
    this._camera.top = viewHeight / 2;
    this._camera.bottom = -viewHeight / 2;
    this._camera.updateProjectionMatrix();
    this._requestFrame();
  }

  _updateVisibility() {
    if (document.hidden || !this._visible) {
      cancelAnimationFrame(this._frame);
      this._frame = 0;
      this._lastFrame = 0;
    } else this._requestFrame();
  }

  _requestFrame() {
    if (this._disposed || this._contextLost || !this._renderer || this._frame || document.hidden || !this._visible) return;
    this._frame = requestAnimationFrame(this._boundTick);
  }

  _animateDetails(elapsed) {
    if (!this._eyes) return;
    const active = !this._reducedMotion;
    let blink = 1;
    if (active && elapsed > this._blinkAt) {
      const since = elapsed - this._blinkAt;
      if (since < 0.18) blink = Math.max(0.08, Math.abs(since - 0.09) / 0.09);
      else this._blinkAt = elapsed + 3.5 + (Math.sin(elapsed * 1.7) + 1) * 1.3;
    }
    const attentive = this._mood === 'listening' ? 1.12 : 1;
    const thinking = this._mood === 'thinking' ? 0.8 : 1;
    this._eyes.forEach((eye, index) => {
      const speaking = active && this._mood === 'speaking' ? 1 + Math.sin(elapsed * 6 + index * 0.6) * 0.045 : 1;
      eye.scale.y = blink * attentive * thinking * speaking;
    });
    // Breathing affects only the torso shell; head pivots remain data-exact.
    this._torso.scale.y = active ? 1 + Math.sin(elapsed * 1.7) * 0.003 : 1;
    const pulse = active ? (Math.sin(elapsed * (this._mood === 'speaking' ? 5 : 2.1)) + 1) / 2 : 0.5;
    this._chestMaterial.emissiveIntensity = 0.08 + pulse * (this._mood === 'idle' ? 0.08 : 0.24);
    this._antennaMaterial.emissiveIntensity = 0.08 + pulse * 0.12;
  }

  _tick(timestamp) {
    this._frame = 0;
    if (this._disposed || this._contextLost || !this._renderer || document.hidden || !this._visible) return;
    // Thirty frames per second is enough for eye animation; PTZ updates are immediate.
    const delta = this._lastFrame ? Math.min((timestamp - this._lastFrame) / 1000, 0.1) : 0;
    if (this._lastFrame && delta < 1 / 32 && !this._reducedMotion) { this._requestFrame(); return; }
    this._lastFrame = timestamp;
    this._elapsed += delta;
    this._animateDetails(this._elapsed);
    this._renderer.render(this._scene, this._camera);
    if (!this._reducedMotion) this._requestFrame();
  }

  _showError(code, error) {
    this.dataset.renderState = 'error';
    this._fallback.hidden = false;
    this._fallback.querySelector('strong').textContent = '3D 场景暂时无法显示';
    this._fallback.querySelector('span:last-child').textContent = '请启用浏览器硬件加速，或重新打开页面。摄像头状态仍可在页面查看。';
    this.dispatchEvent(new CustomEvent('error', { bubbles: true, composed: true,
      detail: { code, fatal: true, message: error instanceof Error ? error.message : String(error) } }));
  }

  _disposeScene() {
    const materials = new Set();
    const textures = new Set();
    const geometries = new Set();
    this._scene?.traverse(object => {
      if (object.geometry) geometries.add(object.geometry);
      if (object.material) for (const mat of Array.isArray(object.material) ? object.material : [object.material]) materials.add(mat);
      object.shadow?.dispose();
    });
    for (const mat of materials) {
      for (const value of Object.values(mat)) if (value?.isTexture) textures.add(value);
      mat.dispose();
    }
    for (const texture of textures) texture.dispose();
    for (const geometry of geometries) geometry.dispose();
    this._environment?.dispose();
    if (this._renderer) {
      this._renderer.dispose();
      this._renderer.forceContextLoss();
      this._renderer.domElement.remove();
    }
    this._renderer = null;
    this._scene = null;
    this._environment = null;
    this._yawPivot = null;
    this._pitchPivot = null;
    this._eyes = null;
    // Mesh.parent links otherwise retain the entire disposed scene while a
    // detached custom element remains held by the UI's component reference.
    this._torso = null;
    this._smile = null;
    this._camera = null;
    this._eyeMaterial = null;
    this._chestMaterial = null;
    this._antennaMaterial = null;
    this._pointer = null;
    this._lastFrame = 0;
  }
}

if (!customElements.get('ai-tutor-robot')) customElements.define('ai-tutor-robot', AiTutorRobot);
