// No browser, server, camera or GPU is started. Actual Three.js scene objects are
// used with an event/RAF harness and a renderer sink for lifecycle transitions.
import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import * as THREE from '../vendor/three/three.module.min.js';

class TestNode extends EventTarget {
  constructor() {
    super();
    this.attributes = new Map();
    this.listeners = new Map();
    this.classList = { add() {}, remove() {} };
    this.hidden = false;
  }
  addEventListener(name, handler, options) {
    super.addEventListener(name, handler, options);
    if (!this.listeners.has(name)) this.listeners.set(name, new Set());
    this.listeners.get(name).add(handler);
  }
  removeEventListener(name, handler, options) {
    super.removeEventListener(name, handler, options);
    this.listeners.get(name)?.delete(handler);
  }
  get listenerCount() { return [...this.listeners.values()].reduce((sum, handlers) => sum + handlers.size, 0); }
  setAttribute(name, value) {
    const old = this.getAttribute(name);
    this.attributes.set(name, String(value));
    this.attributeChangedCallback?.(name, old, String(value));
  }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  prepend(child) { child.parentNode = this; }
  getBoundingClientRect() { return { left: 0, top: 0, width: 600, height: 420 }; }
  setPointerCapture(id) { this.pointerId = id; }
  remove() { this.parentNode = null; this.removed = true; }
  closest() { return null; }
}

const registered = new Map();
globalThis.HTMLElement = class extends TestNode {
  constructor() { super(); this.dataset = {}; }
  attachShadow() {
    const nodes = new Map(['.stage', '.fallback', '#robot-help'].map(key => [key, new TestNode()]));
    const errorNodes = new Map(['strong', 'span:last-child'].map(key => [key, new TestNode()]));
    nodes.get('.fallback').querySelector = key => errorNodes.get(key);
    this.shadowRoot = { innerHTML: '', querySelector: key => nodes.get(key) };
  }
  getBoundingClientRect() { return { width: 600, height: 420 }; }
};
globalThis.customElements = { get: name => registered.get(name), define: (name, constructor) => registered.set(name, constructor) };
globalThis.document = new TestNode();
const media = new TestNode();
media.matches = false;
globalThis.window = { devicePixelRatio: 1.5, matchMedia: () => media };
class TestObserver {
  constructor(callback) { this.callback = callback; }
  observe(target) { this.target = target; this.disconnected = false; }
  disconnect() { this.disconnected = true; this.target = null; }
}
globalThis.ResizeObserver = TestObserver;
globalThis.IntersectionObserver = TestObserver;
const frames = new Map();
let nextFrame = 1;
globalThis.requestAnimationFrame = callback => { const id = nextFrame++; frames.set(id, callback); return id; };
globalThis.cancelAnimationFrame = id => frames.delete(id);
function flushFrame(timestamp = 100) {
  const scheduled = [...frames.values()];
  frames.clear();
  for (const callback of scheduled) callback(timestamp);
}

await import('../robot-companion.js');
const Robot = registered.get('ai-tutor-robot');

class RendererSink {
  constructor() {
    this.domElement = new TestNode();
    this.xr = { enabled: false };
    this.state = { buffers: { depth: { getReversed: () => false } } };
    this.autoClear = true;
    this.clearColor = new THREE.Color();
    this.clearAlpha = 0;
    this.renderCount = 0;
  }
  setPixelRatio(value) { this.pixelRatio = value; }
  setSize(width, height) { this.size = [width, height]; }
  render() { this.renderCount++; }
  compile() {}
  getRenderTarget() { return this.target ?? null; }
  setRenderTarget(target) { this.target = target; }
  getActiveCubeFace() { return 0; }
  getActiveMipmapLevel() { return 0; }
  getClearColor(result) { return result.copy(this.clearColor); }
  setClearColor(value, alpha) { this.clearColor.set(value); if (alpha !== undefined) this.clearAlpha = alpha; }
  getClearAlpha() { return this.clearAlpha; }
  setClearAlpha(value) { this.clearAlpha = value; }
  dispose() { this.disposed = true; }
  forceContextLoss() { this.contextReleased = true; }
}

class LifecycleRobot extends Robot {
  _buildScene() {
    this._scene = new THREE.Scene();
    this._camera = new THREE.OrthographicCamera();
    this._renderer = new RendererSink();
    this._stage.prepend(this._renderer.domElement);
    this._environment = new THREE.WebGLRenderTarget(1, 1);
    const body = new THREE.Group();
    this._scene.add(body);
    this._torso = new THREE.Mesh(new THREE.BoxGeometry(), new THREE.MeshStandardMaterial());
    body.add(this._torso);
    this._yawPivot = new THREE.Group();
    this._pitchPivot = new THREE.Group();
    body.add(this._yawPivot);
    this._yawPivot.add(this._pitchPivot);
    this._smile = new THREE.Mesh(new THREE.BoxGeometry(), new THREE.MeshBasicMaterial());
    this._pitchPivot.add(this._smile);
    this._eyeMaterial = new THREE.MeshBasicMaterial();
    this._chestMaterial = new THREE.MeshStandardMaterial();
    this._antennaMaterial = new THREE.MeshStandardMaterial();
    this._eyes = [new THREE.Group(), new THREE.Group()];
    this._pupils = [new THREE.Group(), new THREE.Group()];
    this._brows = [new THREE.Mesh(new THREE.BoxGeometry(), new THREE.MeshBasicMaterial()), new THREE.Mesh(new THREE.BoxGeometry(), new THREE.MeshBasicMaterial())];
    this._mouthOpen = new THREE.Mesh(new THREE.BoxGeometry(), new THREE.MeshBasicMaterial());
    this._pitchPivot.add(this._mouthOpen);
    this._applyHeadPose();
    this._updateCamera();
  }
}

const active = new Set();
function createRobot(connect = false) {
  const robot = new LifecycleRobot();
  active.add(robot);
  if (connect) robot.connectedCallback();
  return robot;
}
const live = (session = 'A', pan = 24, tilt = 12) => ({
  source: 'feedback', connected: true, fresh: true, camera_session_id: session,
  pan_deg: pan, tilt_deg: tilt, neutral_pan_deg: 0, neutral_tilt_deg: 0,
  pan_sign: -1, tilt_sign: 1, revision: `${session}:${pan}:${tilt}`,
});
const unknown = session => ({ ...live(session), source: 'unknown', connected: false, fresh: false, pan_deg: null, tilt_deg: null });
const demo = session => ({ ...live(session, 60, 30), source: 'demo', connected: false });
const pose = (robot, sample) => robot.setAttribute('pose', JSON.stringify(sample));
const angles = robot => robot.getPoseSnapshot().rendered;

afterEach(() => {
  for (const robot of active) robot.disconnectedCallback();
  active.clear();
  frames.clear();
  document.hidden = false;
  media.matches = false;
});

test('same-session live data, stale hold, reconnect and demo exit preserve real pose', () => {
  const robot = createRobot();
  pose(robot, live());
  assert.deepEqual(angles(robot), { yaw_deg: -24, pitch_deg: 12 });
  pose(robot, unknown('A'));
  assert.deepEqual(angles(robot), { yaw_deg: -24, pitch_deg: 12 });
  pose(robot, live('A', -17, 18));
  pose(robot, demo('A'));
  assert.deepEqual(angles(robot), { yaw_deg: -60, pitch_deg: 30 });
  pose(robot, unknown('A'));
  assert.deepEqual(angles(robot), { yaw_deg: 17, pitch_deg: 18 });
});

test('A-live to B-unknown clears the old device pose', () => {
  const robot = createRobot();
  pose(robot, live('A'));
  pose(robot, unknown('B'));
  assert.deepEqual(angles(robot), { yaw_deg: 0, pitch_deg: 0 });
  assert.equal(robot.getPoseSnapshot().accepted.camera_session_id, 'B');
  assert.equal(robot._lastLivePose, null);
  pose(robot, live('B', 7, 9));
  assert.deepEqual(angles(robot), { yaw_deg: -7, pitch_deg: 9 });
});

test('A-live to B-demo then exit never restores A', () => {
  const robot = createRobot();
  pose(robot, live('A'));
  pose(robot, demo('B'));
  assert.deepEqual(angles(robot), { yaw_deg: -60, pitch_deg: 30 });
  pose(robot, unknown('B'));
  assert.deepEqual(angles(robot), { yaw_deg: 0, pitch_deg: 0 });
  assert.equal(robot.getPoseSnapshot().accepted.camera_session_id, 'B');
});

test('hidden pages and offscreen elements stop RAF, then display the newest pose', () => {
  const robot = createRobot(true);
  const renderer = robot._renderer;
  flushFrame();
  document.hidden = true;
  document.dispatchEvent(new Event('visibilitychange'));
  assert.equal(frames.size, 0);
  const before = renderer.renderCount;
  pose(robot, live('A', 42, 18));
  assert.equal(frames.size, 0);
  assert.equal(renderer.renderCount, before);
  document.hidden = false;
  document.dispatchEvent(new Event('visibilitychange'));
  flushFrame(200);
  assert.equal(renderer.renderCount, before + 1);
  assert.ok(Math.abs(robot._yawPivot.rotation.y + 42 * Math.PI / 180) < 1e-12);
  robot._intersectionObserver.callback([{ isIntersecting: false }]);
  assert.equal(frames.size, 0);
  robot._intersectionObserver.callback([{ isIntersecting: true }]);
  assert.equal(frames.size, 1);
});

test('WebGL loss pauses rendering; restore rebuilds environment and keeps current pose', () => {
  const robot = createRobot(true);
  const errors = [];
  robot.addEventListener('error', event => errors.push(event.detail));
  const renderer = robot._renderer;
  const oldEnvironment = robot._environment;
  const lost = new Event('webglcontextlost', { cancelable: true });
  renderer.domElement.dispatchEvent(lost);
  assert.equal(lost.defaultPrevented, true);
  assert.equal(robot.dataset.renderState, 'error');
  assert.equal(frames.size, 0);
  pose(robot, live('A', 33, 18));
  assert.equal(frames.size, 0);
  renderer.domElement.dispatchEvent(new Event('webglcontextrestored'));
  assert.equal(robot.dataset.renderState, 'ready', JSON.stringify(errors));
  assert.notEqual(robot._environment, oldEnvironment);
  assert.equal(robot._scene.environment, robot._environment.texture);
  assert.deepEqual(angles(robot), { yaw_deg: -33, pitch_deg: 18 });
  assert.equal(frames.size, 1);
});

test('unmount releases scene references, listeners and RAF; remount uses latest real pose', () => {
  const robot = createRobot(true);
  pose(robot, live('A'));
  const renderer = robot._renderer;
  const resizeObserver = robot._resizeObserver;
  const intersectionObserver = robot._intersectionObserver;
  const oldScene = robot._scene;
  assert.equal(robot._torso.parent.parent, oldScene);
  robot.disconnectedCallback();
  assert.equal(renderer.disposed, true);
  assert.equal(renderer.contextReleased, true);
  assert.equal(renderer.domElement.removed, true);
  assert.equal(renderer.domElement.listenerCount, 0);
  assert.equal(document.listenerCount, 0);
  assert.equal(media.listenerCount, 0);
  assert.equal(resizeObserver.disconnected, true);
  assert.equal(intersectionObserver.disconnected, true);
  assert.equal(frames.size, 0);
  for (const key of ['_scene', '_torso', '_smile', '_camera', '_eyeMaterial', '_chestMaterial', '_antennaMaterial', '_bodyMotion', '_expressionPivot', '_arms', '_wrists', '_pupils', '_brows', '_antenna', '_mouthOpen']) {
    assert.ok(robot[key] === null, `${key} must not retain the disposed scene`);
  }
  pose(robot, live('A', -10, 20));
  robot.connectedCallback();
  assert.notEqual(robot._renderer, renderer);
  assert.notEqual(robot._scene, oldScene);
  assert.deepEqual(angles(robot), { yaw_deg: 10, pitch_deg: 20 });
});

test('moods and greetings animate expression without altering the accepted head pose', () => {
  const robot = createRobot(true);
  pose(robot, live('A', 24, 12));
  const before = angles(robot);
  robot.setAttribute('mood', 'listening');
  robot._animateDetails(1);
  assert.equal(robot._eyes[0].scale.y, 1.12);
  assert.equal(robot._brows[0].visible, true);
  robot.setAttribute('mood', 'thinking');
  robot._animateDetails(2);
  assert.equal(robot._eyes[0].scale.y, 0.82);
  assert.ok(robot._gaze.x > 0);
  robot.setAttribute('mood', 'speaking');
  robot._animateDetails(3);
  assert.equal(robot._smile.visible, false);
  assert.equal(robot._mouthOpen.visible, true);
  robot.setAttribute('mood', 'idle');
  robot.greet();
  robot._animateDetails(1.4);
  assert.equal(robot.getPoseSnapshot().gesture, 'hello');
  assert.deepEqual(angles(robot), before);
  robot._animateDetails(4.3);
  assert.equal(robot.getPoseSnapshot().gesture, 'rest');
  assert.deepEqual(angles(robot), before);
});

test('reduced motion stops greeting and gaze movement without losing mood feedback', () => {
  const robot = createRobot(true);
  robot.greet();
  assert.equal(robot.getPoseSnapshot().gesture, 'hello');
  media.matches = true;
  media.dispatchEvent(new Event('change'));
  robot.setAttribute('mood', 'listening');
  assert.equal(robot.getPoseSnapshot().gesture, 'rest');
  assert.equal(robot._eyes[0].scale.y, 1.12);
  robot.greet();
  assert.equal(robot.getPoseSnapshot().gesture, 'rest');
  assert.deepEqual(robot._gazeTarget, { x: 0, y: 0 });
});


test('canvas taps and Enter greet while dragging changes only the viewer', () => {
  const robot = createRobot(true);
  pose(robot, live('A', 24, 12));
  const before = angles(robot);
  const canvas = robot._renderer.domElement;
  const pointer = (type, x, y) => {
    const event = new Event(type);
    Object.assign(event, { isPrimary: true, button: 0, pointerId: 1, clientX: x, clientY: y });
    canvas.dispatchEvent(event);
  };
  pointer('pointerdown', 300, 200);
  pointer('pointerup', 300, 200);
  assert.equal(robot.getPoseSnapshot().gesture, 'hello');
  assert.deepEqual(angles(robot), before);
  robot._animateDetails(robot._elapsed + 5);
  pointer('pointerdown', 300, 200);
  pointer('pointermove', 340, 220);
  pointer('pointerup', 340, 220);
  assert.equal(robot.getPoseSnapshot().gesture, 'rest');
  assert.notEqual(robot.getPoseSnapshot().view.azimuth_deg, 0.22 * 180 / Math.PI);
  assert.deepEqual(angles(robot), before);
  const enter = new Event('keydown', { cancelable: true });
  Object.assign(enter, { key: 'Enter', repeat: false });
  canvas.dispatchEvent(enter);
  assert.equal(enter.defaultPrevented, true);
  assert.equal(robot.getPoseSnapshot().gesture, 'hello');
  assert.deepEqual(angles(robot), before);
});
