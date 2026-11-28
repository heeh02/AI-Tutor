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
 * Tap/Enter or greet() plays a local wave without changing the head's hardware
 * alignment. Moods reflect recording, generation and playback, not audio volume.
 * Public methods: resetView(), greet(), getPoseSnapshot(). Events: ready, error.
 */
import * as THREE from './vendor/three/three.module.min.js';

const DEG = Math.PI / 180;
const SOURCES = new Set(['feedback', 'commanded', 'demo']);
const MOODS = new Set(['idle', 'listening', 'thinking', 'speaking']);
const A11Y_COPY = Object.freeze({
  zh: {
    label: '奶白色 3D 机器人学习伙伴',
    help: '可拖动或用方向键观察机器人，轻点或按 Enter 打招呼。双击或按 Home 键恢复视角。视角操作不会移动摄像头。机器人头部姿态由页面提供的数据驱动。',
  },
  en: {
    label: 'Cream-colored 3D robot learning companion',
    help: 'Drag or use the arrow keys to view the robot. Tap or press Enter to say hello. Double-click or press Home to reset the view. View controls do not move the physical camera. The robot head follows pose data supplied by the page.',
  },
});
const DEFAULT_VIEW = Object.freeze({ azimuth: 0.22, elevation: 0.11 });
const PALETTE = Object.freeze({
  shell: 0xfff7e9, lavender: 0x8fb2c9, dark: 0x20384d,
  coral: 0xd7ad57, eye: 0xa9f1ed, violet: 0xe3b85e, joint: 0x8698a8,
});

const STYLE = `
  :host { display:block; position:relative; width:100%; height:100%; min-height:var(--robot-min-height,0px); contain:layout paint; }
  .stage { position:absolute; inset:0; overflow:hidden; border-radius:inherit; }
  canvas { display:block; width:100%; height:100%; outline:none; cursor:grab; touch-action:pan-y; -webkit-tap-highlight-color:transparent; }
  canvas.dragging { cursor:grabbing; }
  canvas:focus-visible { outline:2px solid #759abb; outline-offset:-5px; border-radius:18px; }
  .fallback { position:absolute; inset:0; display:grid; place-content:center; gap:12px; padding:32px; text-align:center; color:#5d7183; font:14px/1.7 system-ui,sans-serif; }
  .fallback[hidden] { display:none; }
  .fallback strong { display:block; color:#27465e; font-size:17px; font-weight:600; }
  .fallback-icon { width:56px; height:56px; margin:0 auto; display:grid; place-items:center; border-radius:18px; background:#e7f0f7; font-size:26px; }
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
  scene.background = new THREE.Color(0xdbe5eb);
  const shell = new THREE.Mesh(new THREE.BoxGeometry(12, 12, 12),
    new THREE.MeshBasicMaterial({ color: 0x667c8d, side: THREE.BackSide }));
  scene.add(shell);
  for (const [x, y, z, width, height, color, intensity] of [
    [-3, 3, 3, 3.5, 5, 0xfff5e7, 5], [4, 1, 0, 2, 5, 0xddeef8, 3],
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
    this._blinkAt = 2.7;
    this._gesture = null;
    this._gaze = { x: 0, y: 0 };
    this._gazeTarget = { x: 0, y: 0 };
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
      if (this._reducedMotion) {
        this._gesture = null;
        this._gazeTarget = { x: 0, y: 0 };
        this.dataset.gesture = 'rest';
      }
      this._animateDetails(this._elapsed);
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
    this._gazeTarget = { x: 0, y: 0 };
    this._updateCamera();
    this._requestFrame();
  }

  greet() {
    if (this._disposed || this._contextLost || this._reducedMotion) return;
    this._gesture = { name: 'hello', started: this._elapsed, duration: 4 };
    this.dataset.gesture = 'hello';
    this._requestFrame();
  }

  getPoseSnapshot() {
    return {
      accepted: { ...this._pose }, incoming: { ...this._incoming },
      rendered: { yaw_deg: this._headDegrees.yaw, pitch_deg: this._headDegrees.pitch },
      euler: { y: this._yawPivot?.rotation.y ?? this._headDegrees.yaw * DEG,
        x: this._pitchPivot?.rotation.x ?? -this._headDegrees.pitch * DEG, order: 'yaw-then-local-pitch' },
      view: { azimuth_deg: this._view.azimuth / DEG, elevation_deg: this._view.elevation / DEG },
      mood: this._mood, gesture: this._gesture?.name ?? 'rest',
      gaze: { ...this._gaze }, renderer: this.dataset.renderState ?? 'pending',
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
    // Hardware alignment has one data path; local expressions use child pivots.
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
    const next = MOODS.has(value) ? value : 'idle';
    this._mood = next;
    this.dataset.mood = next;
    if (this._eyeMaterial) {
      this._eyeMaterial.color.setHex(this._mood === 'thinking' ? PALETTE.violet : PALETTE.eye);
      this._chestMaterial.color.setHex(this._mood === 'speaking' ? PALETTE.coral : PALETTE.lavender);
      this._chestMaterial.emissive.copy(this._chestMaterial.color);
      this._animateDetails(this._elapsed);
    }
    this._requestFrame();
  }

  _updateFallbackLanguage() {
    const english = this.getAttribute('lang') === 'en';
    this._fallback.querySelector('strong').textContent = english ? 'The 3D companion is unavailable' : '3D 场景暂时无法显示';
    this._fallback.querySelector('span:last-child').textContent = english
      ? 'Enable hardware acceleration or reopen this page. Camera status remains available on this page.'
      : '请启用浏览器硬件加速，或重新打开页面。摄像头状态仍可在页面查看。';
  }

  _readLanguage() {
    const copy = A11Y_COPY[this.getAttribute('lang') === 'en' ? 'en' : 'zh'];
    this.shadowRoot.querySelector('#robot-help').textContent = copy.help;
    this._renderer?.domElement.setAttribute('aria-label', copy.label);
    this._updateFallbackLanguage();
  }

  _buildScene() {
    this._scene = new THREE.Scene();
    this._camera = new THREE.OrthographicCamera(-2.5, 2.5, 2.2, -2.2, 0.1, 30);
    this._renderer = new THREE.WebGLRenderer({ alpha: true, antialias: true, powerPreference: 'low-power' });
    this._renderer.setClearColor(0x000000, 0);
    this._renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.75));
    this._renderer.outputColorSpace = THREE.SRGBColorSpace;
    this._renderer.toneMapping = THREE.ACESFilmicToneMapping;
    this._renderer.toneMappingExposure = 1.06;
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
    this._scene.environmentIntensity = 0.48;

    this._scene.add(new THREE.HemisphereLight(0xfffbf2, 0x94aabc, 1.5));
    const key = new THREE.DirectionalLight(0xfff1df, 2.7);
    key.position.set(-3, 7, 5);
    key.castShadow = true;
    key.shadow.mapSize.set(1024, 1024);
    Object.assign(key.shadow.camera, { left: -3, right: 3, top: 5, bottom: -2, near: 0.1, far: 15 });
    key.shadow.normalBias = 0.035;
    key.shadow.bias = -0.0002;
    key.shadow.radius = 3;
    this._scene.add(key);
    const fill = new THREE.DirectionalLight(0xd9e9f5, 1.5);
    fill.position.set(4, 2, 2);
    this._scene.add(fill);
    const rim = new THREE.DirectionalLight(0xffffff, 2);
    rim.position.set(1, 4, -4);
    this._scene.add(rim);

    const shell = roundedMaterial(PALETTE.shell, {
      roughness: 0.28, metalness: 0.025, clearcoat: 0.58, clearcoatRoughness: 0.2,
    });
    const lavender = roundedMaterial(PALETTE.lavender, {
      roughness: 0.38, metalness: 0.08, clearcoat: 0.5, clearcoatRoughness: 0.25,
    });
    const joint = roundedMaterial(PALETTE.joint, {
      metalness: 0.52, roughness: 0.31, clearcoat: 0.28,
    });
    const coral = roundedMaterial(PALETTE.coral, { roughness: 0.38, clearcoat: 0.48 });
    const dark = roundedMaterial(PALETTE.dark, { roughness: 0.2, clearcoat: 0.95, metalness: 0.12 });
    const robot = new THREE.Group();
    this._scene.add(robot);

    // Layered soles stay planted; only the torso and its child joints breathe.
    for (const side of [-1, 1]) {
      const x = side * 0.34;
      addMesh(robot, roundedBox(0.53, 0.16, 0.74, 0.16, 0.045), lavender, x, 0.135, 0.12);
      addMesh(robot, roundedBox(0.48, 0.25, 0.65, 0.15, 0.06), shell, x, 0.25, 0.11);
      addMesh(robot, roundedBox(0.4, 0.115, 0.19, 0.075, 0.025), lavender, x, 0.245, 0.395);
      addMesh(robot, roundedBox(0.38, 0.14, 0.12, 0.055, 0.025), joint, x, 0.19, -0.245);
      addMesh(robot, new THREE.CylinderGeometry(0.13, 0.145, 0.27, 32), joint, side * 0.32, 0.475, 0);
      addMesh(robot, new THREE.TorusGeometry(0.137, 0.022, 10, 32), lavender,
        side * 0.32, 0.38, 0).rotation.x = Math.PI / 2;
    }
    this._bodyMotion = new THREE.Group();
    robot.add(this._bodyMotion);
    const body = this._bodyMotion;
    this._torso = addMesh(body, roundedBox(1.1, 1.14, 0.81, 0.28, 0.1), shell, 0, 1.09, 0);
    addMesh(body, roundedBox(0.7, 0.58, 0.07, 0.18, 0.025), lavender, 0, 1.11, 0.42);
    addMesh(body, roundedBox(0.6, 0.49, 0.055, 0.15, 0.02), shell, 0, 1.12, 0.46);
    this._chestMaterial = roundedMaterial(PALETTE.lavender, { emissive: PALETTE.lavender, emissiveIntensity: 0.15 });
    const heart = new THREE.Shape();
    heart.moveTo(0, -0.095);
    heart.bezierCurveTo(-0.18, 0.025, -0.105, 0.16, 0, 0.075);
    heart.bezierCurveTo(0.105, 0.16, 0.18, 0.025, 0, -0.095);
    addMesh(body, new THREE.ExtrudeGeometry(heart, { depth: 0.018, bevelEnabled: true,
      bevelThickness: 0.008, bevelSize: 0.008, bevelSegments: 3, steps: 1, curveSegments: 16 }),
    this._chestMaterial, 0, 1.14, 0.496);
    this._arms = [];
    this._elbows = [];
    this._wrists = [];
    for (const side of [-1, 1]) {
      addMesh(body, new THREE.SphereGeometry(0.17, 28, 20), lavender, side * 0.6, 1.35, 0);
      const shoulderRing = addMesh(body, new THREE.TorusGeometry(0.145, 0.025, 10, 32), joint,
        side * 0.635, 1.34, 0);
      shoulderRing.rotation.y = Math.PI / 2;
      const arm = new THREE.Group();
      arm.position.set(side * 0.64, 1.34, 0.015);
      arm.userData.side = side;
      body.add(arm);
      const upperArm = addMesh(arm, new THREE.CapsuleGeometry(0.132, 0.14, 8, 24), shell, 0, -0.155, 0);
      upperArm.scale.z = 0.94;
      const elbow = new THREE.Group();
      elbow.position.set(0, -0.34, 0);
      arm.add(elbow);
      addMesh(elbow, new THREE.SphereGeometry(0.126, 24, 16), joint);
      const forearm = addMesh(elbow, new THREE.CapsuleGeometry(0.118, 0.12, 8, 24), shell, 0, -0.135, 0.015);
      forearm.scale.z = 0.94;
      const wrist = new THREE.Group();
      wrist.position.set(0, -0.29, 0.02);
      elbow.add(wrist);
      addMesh(wrist, new THREE.CylinderGeometry(0.105, 0.115, 0.075, 24), joint, 0, 0.035, 0);
      const hand = addMesh(wrist, new THREE.SphereGeometry(0.15, 28, 20), lavender, 0, -0.035, 0.005);
      hand.scale.set(0.96, 1.05, 0.88);
      const thumb = addMesh(wrist, new THREE.SphereGeometry(0.078, 20, 14), shell, -side * 0.105, -0.015, 0.075);
      thumb.scale.set(0.85, 1, 0.9);
      this._arms.push(arm);
      this._elbows.push(elbow);
      this._wrists.push(wrist);
    }
    addMesh(body, new THREE.CylinderGeometry(0.22, 0.25, 0.2, 40), joint, 0, 1.73, 0);
    addMesh(body, new THREE.TorusGeometry(0.235, 0.035, 12, 40), lavender, 0, 1.73, 0).rotation.x = Math.PI / 2;

    // Hardware owns yaw and pitch; expression tilt lives in its own child pivot.
    this._yawPivot = new THREE.Group();
    this._yawPivot.position.y = 1.81;
    body.add(this._yawPivot);
    this._pitchPivot = new THREE.Group();
    this._yawPivot.add(this._pitchPivot);
    this._expressionPivot = new THREE.Group();
    this._pitchPivot.add(this._expressionPivot);
    const head = this._expressionPivot;
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
    this._antenna = new THREE.Group();
    this._antenna.position.set(0, 1.18, -0.13);
    head.add(this._antenna);
    addMesh(this._antenna, new THREE.CylinderGeometry(0.055, 0.075, 0.22, 24), lavender, 0, 0.11, 0);
    this._antennaMaterial = roundedMaterial(PALETTE.coral, { emissive: PALETTE.coral, emissiveIntensity: 0.12 });
    addMesh(this._antenna, new THREE.SphereGeometry(0.11, 24, 16), this._antennaMaterial, 0, 0.25, 0);
    addMesh(this._antenna, new THREE.SphereGeometry(0.028, 16, 12),
      new THREE.MeshBasicMaterial({ color: 0xffe7ac }), -0.028, 0.287, 0.074);
    const lensBase = addMesh(head, new THREE.CylinderGeometry(0.075, 0.075, 0.025, 28), lavender, 0, 1.08, 0.594);
    lensBase.rotation.x = Math.PI / 2;
    const lens = addMesh(head, new THREE.CylinderGeometry(0.044, 0.044, 0.029, 28), dark, 0, 1.08, 0.615);
    lens.rotation.x = Math.PI / 2;
    addMesh(head, new THREE.SphereGeometry(0.014, 16, 10), new THREE.MeshBasicMaterial({ color: 0xc6e3f2 }), -0.012, 1.094, 0.637);

    this._eyeMaterial = new THREE.MeshBasicMaterial({ color: PALETTE.eye, toneMapped: false });
    const eyeRim = new THREE.MeshBasicMaterial({ color: 0x82aec5, toneMapped: false });
    const eyeHighlight = new THREE.MeshBasicMaterial({ color: 0xf1fffc, toneMapped: false });
    this._eyes = [];
    this._pupils = [];
    this._brows = [];
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
      const pupil = new THREE.Group();
      eye.add(pupil);
      addMesh(pupil, roundedBox(0.164, 0.28, 0.018, 0.077, 0.006), this._eyeMaterial, 0.008, 0.006, 0.011);
      addMesh(pupil, new THREE.SphereGeometry(0.027, 16, 12), eyeHighlight, -0.035, 0.092, 0.027).scale.z = 0.2;
      addMesh(pupil, new THREE.SphereGeometry(0.012, 12, 8), eyeHighlight, 0.047, -0.071, 0.026).scale.z = 0.18;
      this._eyes.push(eye);
      this._pupils.push(pupil);
      const brow = addMesh(head, roundedBox(0.15, 0.024, 0.008, 0.01, 0.002),
        new THREE.MeshBasicMaterial({ color: 0x8eafc2, toneMapped: false }), side * 0.32, 0.83, 0.704);
      brow.visible = false;
      this._brows.push(brow);
    }
    const smileCurve = new THREE.QuadraticBezierCurve3(
      new THREE.Vector3(-0.12, 0.335, 0.709), new THREE.Vector3(0, 0.245, 0.715),
      new THREE.Vector3(0.12, 0.335, 0.709));
    this._smile = addMesh(head, new THREE.TubeGeometry(smileCurve, 28, 0.014, 8, false),
      new THREE.MeshBasicMaterial({ color: 0xa9c3d2, toneMapped: false }));
    this._mouthOpen = new THREE.Group();
    this._mouthOpen.position.set(0, 0.29, 0.714);
    this._mouthOpen.scale.set(0.067, 0.075, 0.008);
    this._mouthOpen.visible = false;
    head.add(this._mouthOpen);
    addMesh(this._mouthOpen, new THREE.SphereGeometry(1, 28, 18),
      new THREE.MeshBasicMaterial({ color: 0x365266, toneMapped: false }));
    const tongue = addMesh(this._mouthOpen, new THREE.SphereGeometry(1, 20, 14),
      new THREE.MeshBasicMaterial({ color: 0xd79b78, toneMapped: false }), 0, -0.38, 0.82);
    tongue.scale.set(0.62, 0.32, 0.22);
    const tooth = addMesh(this._mouthOpen, roundedBox(0.75, 0.24, 0.12, 0.1, 0.035),
      new THREE.MeshBasicMaterial({ color: 0xfff7e9, toneMapped: false }), 0, 0.57, 1.02);
    tooth.scale.set(0.72, 0.72, 0.5);
    this._cheekMaterial = new THREE.MeshBasicMaterial({
      color: 0xd59b77, transparent: true, opacity: 0.42, toneMapped: false,
    });
    for (const side of [-1, 1]) {
      const cheek = addMesh(head, new THREE.SphereGeometry(1, 20, 12), this._cheekMaterial,
        side * 0.56, 0.36, 0.692);
      cheek.scale.set(0.075, 0.022, 0.004);
    }
    // A soft reflected strip makes the face read as a curved glass screen.
    const reflection = new THREE.QuadraticBezierCurve3(
      new THREE.Vector3(-0.53, 0.86, 0.689), new THREE.Vector3(-0.25, 0.92, 0.699),
      new THREE.Vector3(0.12, 0.89, 0.693));
    addMesh(head, new THREE.TubeGeometry(reflection, 24, 0.012, 6, false),
      new THREE.MeshBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0.07, depthWrite: false }));

    // Real shadow receiver plus a soft ambient contact shadow, on a transparent stage.
    const floor = addMesh(this._scene, new THREE.PlaneGeometry(30, 30),
      new THREE.ShadowMaterial({ color: 0x4d6475, opacity: 0.075 }), 0, 0.055, 0);
    floor.rotation.x = -Math.PI / 2;
    floor.castShadow = false;
    const shadow = addMesh(this._scene, new THREE.PlaneGeometry(3.4, 2.65), new THREE.MeshBasicMaterial({
      map: makeSoftTexture('70,93,110', [[0, 0.19], [0.32, 0.12], [0.65, 0.03], [1, 0]]),
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
      this._pointer = { id: event.pointerId, x: event.clientX, y: event.clientY,
        startX: event.clientX, startY: event.clientY, startedAt: performance.now(), moved: false };
      canvas.setPointerCapture(event.pointerId);
      canvas.classList.add('dragging');
    });
    this._listen(canvas, 'pointermove', event => {
      const bounds = canvas.getBoundingClientRect();
      if (!this._reducedMotion && bounds.width > 0 && bounds.height > 0) {
        this._gazeTarget = {
          x: THREE.MathUtils.clamp((event.clientX - bounds.left) / bounds.width * 2 - 1, -1, 1) * 0.044,
          y: THREE.MathUtils.clamp(1 - (event.clientY - bounds.top) / bounds.height * 2, -1, 1) * 0.024,
        };
      }
      if (this._pointer?.id !== event.pointerId) { this._requestFrame(); return; }
      const dx = event.clientX - this._pointer.x;
      const dy = event.clientY - this._pointer.y;
      if (Math.hypot(event.clientX - this._pointer.startX, event.clientY - this._pointer.startY) > 6) this._pointer.moved = true;
      this._pointer.x = event.clientX;
      this._pointer.y = event.clientY;
      this._view.azimuth = THREE.MathUtils.clamp(this._view.azimuth - dx * 0.006, -1.2, 1.2);
      this._view.elevation = THREE.MathUtils.clamp(this._view.elevation + dy * 0.004, -0.06, 0.52);
      this._updateCamera();
      this._requestFrame();
    });
    const endDrag = () => {
      const tapped = this._pointer && !this._pointer.moved && performance.now() - this._pointer.startedAt < 500;
      this._pointer = null;
      canvas.classList.remove('dragging');
      if (tapped) this.greet();
    };
    const cancelDrag = () => { this._pointer = null; canvas.classList.remove('dragging'); };
    this._listen(canvas, 'pointerup', endDrag);
    this._listen(canvas, 'pointercancel', cancelDrag);
    this._listen(canvas, 'lostpointercapture', cancelDrag);
    this._listen(canvas, 'pointerleave', () => {
      if (!this._pointer) { this._gazeTarget = { x: 0, y: 0 }; this._requestFrame(); }
    });
    this._listen(canvas, 'dblclick', () => this.resetView());
    this._listen(canvas, 'keydown', event => {
      if (event.key === 'Home' || event.key === '0') { event.preventDefault(); this.resetView(); return; }
      if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); if (!event.repeat) this.greet(); return; }
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
    const compactHome = this.closest('.home-robot-scene') && height < 275;
    const viewHeight = Math.max(compactHome ? 4.05 : 3.72, 2.8 / aspect);
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
    const ease = active ? 0.16 : 1;
    const listening = this._mood === 'listening';
    const thinking = this._mood === 'thinking';
    const speaking = this._mood === 'speaking';
    let wave = 0;
    let gestureAge = 0;
    if (this._gesture) {
      gestureAge = Math.max(0, elapsed - this._gesture.started);
      const progress = gestureAge / this._gesture.duration;
      if (progress >= 1) {
        this._gesture = null;
        this.dataset.gesture = 'rest';
      } else {
        wave = THREE.MathUtils.smoothstep(progress, 0, 0.16)
          * (1 - THREE.MathUtils.smoothstep(progress, 0.78, 1));
      }
    }
    let blink = 1;
    if (active && elapsed > this._blinkAt) {
      const since = elapsed - this._blinkAt;
      if (since < 0.2) blink = Math.max(0.06, Math.abs(since - 0.1) / 0.1);
      else this._blinkAt = elapsed + 3.8 + (Math.sin(elapsed * 0.83) + 1) * 1.5;
    }
    const attentive = listening ? 1.12 : 1;
    const eyelids = thinking ? 0.82 : speaking ? 0.96 : 1;
    this._eyes.forEach((eye, index) => {
      const talk = active && speaking ? 1 + Math.sin(elapsed * 4.6 + index * 0.45) * 0.018 : 1;
      eye.scale.y = blink * attentive * eyelids * talk * (1 - wave * 0.12);
    });

    const pointerGaze = this._gazeTarget.x !== 0 || this._gazeTarget.y !== 0;
    const idleX = active && this._mood === 'idle' && !pointerGaze ? Math.sin(elapsed * 0.43) * 0.007 : 0;
    const idleY = active && this._mood === 'idle' && !pointerGaze ? Math.sin(elapsed * 0.59) * 0.003 : 0;
    const speakingX = active && speaking && !pointerGaze ? Math.sin(elapsed * 0.72) * 0.003 : 0;
    const targetX = thinking ? 0.032 : wave > 0.05 ? 0 : this._gazeTarget.x + idleX + speakingX;
    const targetY = thinking ? 0.026 : listening && !pointerGaze ? 0.006 : this._gazeTarget.y + idleY;
    this._gaze.x = THREE.MathUtils.lerp(this._gaze.x, active ? targetX : 0, ease);
    this._gaze.y = THREE.MathUtils.lerp(this._gaze.y, active ? targetY : 0, ease);
    this._pupils?.forEach(pupil => pupil.position.set(this._gaze.x, this._gaze.y, 0));
    this._brows?.forEach((brow, index) => {
      const side = index === 0 ? -1 : 1;
      brow.visible = listening || thinking || speaking || wave > 0.04;
      const browY = 0.83 + (listening ? 0.022 : 0) + wave * 0.012 - (thinking && side > 0 ? 0.012 : 0);
      brow.position.y = THREE.MathUtils.lerp(brow.position.y, browY, ease);
      const browTilt = thinking ? (side < 0 ? 0.14 : -0.05) : side * -(listening ? 0.055 : 0.03);
      brow.rotation.z = THREE.MathUtils.lerp(brow.rotation.z, browTilt, ease);
    });
    if (this._smile) {
      this._smile.visible = !speaking || wave > 0.35;
      const smileWidth = thinking ? 0.8 : listening ? 1.04 : 1 + wave * 0.18;
      this._smile.scale.x = THREE.MathUtils.lerp(this._smile.scale.x, smileWidth, ease);
      this._smile.position.y = THREE.MathUtils.lerp(this._smile.position.y, wave * 0.008, ease);
    }
    if (this._mouthOpen) {
      this._mouthOpen.visible = speaking && wave <= 0.35;
      const phrase = active && speaking
        ? (Math.sin(elapsed * 8.6) + 1) * 0.5 * (0.72 + (Math.sin(elapsed * 3.15) + 1) * 0.14)
        : 0.55;
      const mouthY = speaking ? 0.026 + phrase * 0.058 : 0.04;
      const mouthX = speaking ? 0.064 + Math.sin(elapsed * 3.7) * (active ? 0.007 : 0) : 0.06;
      this._mouthOpen.scale.y = THREE.MathUtils.lerp(this._mouthOpen.scale.y, mouthY, active ? 0.34 : 1);
      this._mouthOpen.scale.x = THREE.MathUtils.lerp(this._mouthOpen.scale.x, mouthX, active ? 0.28 : 1);
    }
    if (this._cheekMaterial) {
      const cheekOpacity = speaking || wave > 0.1 ? 0.58 : listening ? 0.48 : 0.4;
      this._cheekMaterial.opacity = THREE.MathUtils.lerp(this._cheekMaterial.opacity, cheekOpacity, ease);
    }

    if (this._bodyMotion) {
      const breath = active ? Math.sin(elapsed * 1.45) * 0.003 : 0;
      const bodyLean = (thinking ? 0.018 : 0) - wave * 0.025
        + (active && speaking ? Math.sin(elapsed * 1.8) * 0.004 : 0);
      this._bodyMotion.rotation.z = THREE.MathUtils.lerp(this._bodyMotion.rotation.z, bodyLean, ease * 0.8);
      this._bodyMotion.rotation.x = THREE.MathUtils.lerp(this._bodyMotion.rotation.x,
        (listening ? 0.024 : 0) + (active && speaking ? Math.sin(elapsed * 2.1) * 0.004 : 0), ease * 0.8);
      this._bodyMotion.position.y = breath;
    }
    if (this._expressionPivot) {
      const idleTilt = active && this._mood === 'idle' ? Math.sin(elapsed * 0.62) * 0.005 : 0;
      const tilt = idleTilt + (thinking ? 0.055 : 0) - (listening ? 0.008 : 0)
        + (active ? Math.sin(gestureAge * 5) * wave * 0.018 : 0);
      this._expressionPivot.rotation.z = THREE.MathUtils.lerp(this._expressionPivot.rotation.z, tilt, ease);
      this._expressionPivot.rotation.x = THREE.MathUtils.lerp(this._expressionPivot.rotation.x,
        (listening ? -0.024 : thinking ? 0.018 : 0)
          + (active && speaking ? Math.sin(elapsed * 2.2) * 0.008 : 0) - wave * 0.01, ease);
    }
    this._arms?.forEach((arm, index) => {
      const side = arm.userData.side;
      let x = active ? Math.sin(elapsed * 1.45 + side * 0.7) * 0.008 : 0;
      let z = side * 0.13 + (active ? Math.sin(elapsed * 1.45 + side * 0.5) * 0.006 : 0);
      let elbowX = -0.055;
      let elbowZ = 0;
      let wristX = 0;
      let wristZ = 0;
      if (listening) {
        x = -0.1;
        z = side * 0.14;
        elbowX = -0.16;
      }
      if (thinking) {
        x = side < 0 ? -0.18 : -0.04;
        z = side < 0 ? -0.22 : 0.13;
        elbowX = side < 0 ? -1.5 : -0.08;
        elbowZ = side < 0 ? 0.12 : 0;
      }
      if (speaking) {
        const phase = elapsed * 2.2 + (side > 0 ? 0 : Math.PI);
        x = -0.1 + (active ? Math.sin(phase) * 0.035 : 0);
        z = side * (0.19 + (active ? (Math.sin(phase) + 1) * 0.025 : 0.025));
        elbowX = -0.18 + (active ? Math.sin(phase + 0.6) * 0.06 : 0);
        wristX = active ? Math.sin(phase) * 0.035 : 0;
      }
      if (wave > 0 && side > 0) {
        x = THREE.MathUtils.lerp(x, -0.04, wave);
        z = THREE.MathUtils.lerp(z, 2.42, wave);
        elbowX = THREE.MathUtils.lerp(elbowX, -0.08, wave);
        elbowZ = THREE.MathUtils.lerp(elbowZ, -0.18, wave);
        wristZ = Math.sin(gestureAge * 11) * wave * 0.26;
      } else if (wave > 0) {
        z += side * wave * 0.035;
        elbowX -= wave * 0.06;
      }
      arm.rotation.x = THREE.MathUtils.lerp(arm.rotation.x, x, ease);
      arm.rotation.z = THREE.MathUtils.lerp(arm.rotation.z, z, ease);
      const elbow = this._elbows[index];
      elbow.rotation.x = THREE.MathUtils.lerp(elbow.rotation.x, elbowX, ease);
      elbow.rotation.z = THREE.MathUtils.lerp(elbow.rotation.z, elbowZ, ease);
      const wrist = this._wrists[index];
      wrist.rotation.x = THREE.MathUtils.lerp(wrist.rotation.x, wristX, ease);
      wrist.rotation.z = THREE.MathUtils.lerp(wrist.rotation.z, wristZ, ease);
    });
    if (this._antenna) {
      this._antenna.rotation.z = active
        ? Math.sin(elapsed * 1.55) * 0.015 + Math.sin(gestureAge * 10) * wave * 0.07 : 0;
    }
    this._torso.scale.y = 1 + (active ? Math.sin(elapsed * 1.45) * 0.002 : 0);
    const pulse = active
      ? (Math.sin(elapsed * (speaking ? 4.2 : thinking ? 1.4 : listening ? 2.4 : 1.8)) + 1) / 2 : 0.5;
    this._chestMaterial.emissiveIntensity = 0.07 + pulse * (this._mood === 'idle' ? 0.06 : 0.18) + wave * 0.1;
    this._antennaMaterial.emissiveIntensity = 0.07 + pulse * (listening ? 0.18 : 0.1) + wave * 0.12;
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
    this._updateFallbackLanguage();
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
    this._expressionPivot = null;
    this._bodyMotion = null;
    this._arms = null;
    this._elbows = null;
    this._wrists = null;
    this._eyes = null;
    this._pupils = null;
    this._brows = null;
    this._antenna = null;
    this._mouthOpen = null;
    this._gesture = null;
    this.dataset.gesture = 'rest';
    this._gaze = { x: 0, y: 0 };
    this._gazeTarget = { x: 0, y: 0 };
    // Mesh.parent links otherwise retain the entire disposed scene while a
    // detached custom element remains held by the UI's component reference.
    this._torso = null;
    this._smile = null;
    this._camera = null;
    this._eyeMaterial = null;
    this._chestMaterial = null;
    this._antennaMaterial = null;
    this._cheekMaterial = null;
    this._pointer = null;
    this._lastFrame = 0;
  }
}

if (!customElements.get('ai-tutor-robot')) customElements.define('ai-tutor-robot', AiTutorRobot);
