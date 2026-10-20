import * as THREE from "./three.module.min.js";

const canvas = document.getElementById("cat-canvas");
const stage = canvas ? canvas.closest(".hero-stage") : null;
const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

if (canvas && stage && !reduced) {
  start(canvas, stage);
}

function start(canvas, stage) {
  let renderer;
  try {
    renderer = new THREE.WebGLRenderer({
      canvas,
      alpha: true,
      antialias: true,
      powerPreference: "high-performance",
    });
  } catch {
    return;
  }

  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
  renderer.setClearColor(0x000000, 0);
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.08;

  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(36, 1, 0.1, 100);
  camera.position.set(0, 0.1, 8.4);

  const cat = new THREE.Group();
  cat.rotation.set(0.03, -0.12, 0);
  cat.scale.setScalar(1.18);
  scene.add(cat);

  const orange = new THREE.MeshStandardMaterial({
    color: 0xe8743b,
    roughness: 0.56,
    metalness: 0.08,
    flatShading: true,
  });
  const cream = new THREE.MeshStandardMaterial({
    color: 0xfff1dc,
    roughness: 0.62,
    metalness: 0.02,
    flatShading: true,
  });
  const pink = new THREE.MeshStandardMaterial({
    color: 0xf29a9e,
    roughness: 0.6,
    metalness: 0.02,
    flatShading: true,
  });
  const teal = new THREE.MeshStandardMaterial({
    color: 0x2f8f8a,
    roughness: 0.45,
    metalness: 0.2,
    emissive: 0x123c3a,
    emissiveIntensity: 0.55,
    flatShading: true,
  });
  const eyeMat = new THREE.MeshStandardMaterial({
    color: 0xffd166,
    roughness: 0.35,
    metalness: 0.05,
    emissive: 0xffb703,
    emissiveIntensity: 0.5,
  });
  const pupilMat = new THREE.MeshStandardMaterial({
    color: 0x1a120c,
    roughness: 0.4,
    metalness: 0.05,
  });

  const headGeo = new THREE.IcosahedronGeometry(1.3, 1);
  const head = new THREE.Mesh(headGeo, orange);
  head.scale.set(1, 0.95, 0.92);
  cat.add(head);

  const edges = new THREE.LineSegments(
    new THREE.EdgesGeometry(headGeo, 16),
    new THREE.LineBasicMaterial({
      color: 0xffe6d2,
      transparent: true,
      opacity: 0.1,
    }),
  );
  edges.scale.copy(head.scale);
  cat.add(edges);

  for (const side of [-1, 1]) {
    const ear = new THREE.Mesh(new THREE.ConeGeometry(0.43, 0.92, 4), orange);
    ear.position.set(side * 0.82, 1.13, -0.02);
    ear.rotation.z = side * -0.3;
    ear.rotation.y = Math.PI * 0.25;
    cat.add(ear);

    const inner = new THREE.Mesh(new THREE.ConeGeometry(0.26, 0.58, 4), pink);
    inner.position.set(side * 0.84, 1.16, 0.14);
    inner.rotation.z = side * -0.3;
    inner.rotation.y = Math.PI * 0.25;
    cat.add(inner);
  }

  const muzzle = new THREE.Mesh(new THREE.SphereGeometry(0.56, 24, 16), cream);
  muzzle.position.set(0, -0.3, 1.02);
  muzzle.scale.set(1.04, 0.62, 0.52);
  cat.add(muzzle);

  const eyes = new THREE.Group();
  cat.add(eyes);
  const pupilBase = [];
  for (const side of [-1, 1]) {
    const eye = new THREE.Mesh(new THREE.SphereGeometry(0.17, 18, 14), eyeMat);
    eye.position.set(side * 0.44, 0.28, 1.08);
    eyes.add(eye);

    const pupil = new THREE.Mesh(new THREE.SphereGeometry(0.074, 14, 12), pupilMat);
    pupil.position.set(side * 0.44, 0.28, 1.26);
    pupil.scale.set(0.8, 1, 0.55);
    eyes.add(pupil);
    pupilBase.push({ mesh: pupil, x: side * 0.44, y: 0.28, z: 1.26 });
  }

  const nose = new THREE.Mesh(new THREE.OctahedronGeometry(0.105, 0), pink);
  nose.position.set(0, -0.1, 1.34);
  nose.rotation.set(0.25, 0, Math.PI * 0.25);
  cat.add(nose);

  const whiskerPositions = [];
  for (const side of [-1, 1]) {
    const anchors = [
      [-0.24, -0.24, 1.28, -1.02, -0.16, 1.34],
      [-0.24, -0.31, 1.27, -1.04, -0.33, 1.3],
      [-0.24, -0.38, 1.24, -0.96, -0.5, 1.26],
    ];
    for (const [x1, y1, z1, x2, y2, z2] of anchors) {
      whiskerPositions.push(side * x1, y1, z1, side * x2, y2, z2);
    }
  }
  const whiskers = new THREE.LineSegments(
    new THREE.BufferGeometry().setAttribute(
      "position",
      new THREE.Float32BufferAttribute(whiskerPositions, 3),
    ),
    new THREE.LineBasicMaterial({ color: 0xfff1dc, transparent: true, opacity: 0.72 }),
  );
  cat.add(whiskers);

  const collar = new THREE.Mesh(new THREE.TorusGeometry(0.8, 0.058, 8, 40), teal);
  collar.position.set(0, -0.86, 0.16);
  collar.rotation.x = Math.PI * 0.42;
  cat.add(collar);

  const tag = new THREE.Mesh(new THREE.OctahedronGeometry(0.13, 0), teal);
  tag.position.set(0, -1.14, 0.58);
  tag.rotation.set(0.2, 0.4, 0);
  cat.add(tag);

  const orbit = new THREE.Group();
  scene.add(orbit);
  const ringA = new THREE.Mesh(
    new THREE.TorusGeometry(2.02, 0.008, 4, 120),
    new THREE.MeshBasicMaterial({ color: 0x2f8f8a, transparent: true, opacity: 0.4 }),
  );
  ringA.rotation.set(1.05, 0.2, 0.18);
  orbit.add(ringA);
  const ringB = new THREE.Mesh(
    new THREE.TorusGeometry(2.46, 0.006, 4, 120),
    new THREE.MeshBasicMaterial({ color: 0xe8743b, transparent: true, opacity: 0.28 }),
  );
  ringB.rotation.set(0.5, 1.12, 0.42);
  orbit.add(ringB);

  const palette = [0xe8743b, 0xffd166, 0xfff1dc, 0x2f8f8a, 0xf29a9e];
  const count = 430;
  const positions = new Float32Array(count * 3);
  const colors = new Float32Array(count * 3);
  const color = new THREE.Color();
  for (let i = 0; i < count; i += 1) {
    const radius = 2.9 + Math.random() * 2.6;
    const theta = Math.random() * Math.PI * 2;
    const phi = Math.acos(2 * Math.random() - 1);
    positions[i * 3] = Math.sin(phi) * Math.cos(theta) * radius;
    positions[i * 3 + 1] = Math.cos(phi) * radius * 0.72;
    positions[i * 3 + 2] = Math.sin(phi) * Math.sin(theta) * radius;
    color.setHex(palette[i % palette.length]);
    colors[i * 3] = color.r;
    colors[i * 3 + 1] = color.g;
    colors[i * 3 + 2] = color.b;
  }
  const particleGeo = new THREE.BufferGeometry();
  particleGeo.setAttribute("position", new THREE.Float32BufferAttribute(positions, 3));
  particleGeo.setAttribute("color", new THREE.Float32BufferAttribute(colors, 3));
  const particles = new THREE.Points(
    particleGeo,
    new THREE.PointsMaterial({
      size: 0.036,
      vertexColors: true,
      transparent: true,
      opacity: 0.72,
      sizeAttenuation: true,
      depthWrite: false,
    }),
  );
  scene.add(particles);

  scene.add(new THREE.HemisphereLight(0xffe6cc, 0x0a0e13, 1.45));
  const key = new THREE.DirectionalLight(0xffd166, 2.7);
  key.position.set(3.2, 4.2, 5);
  scene.add(key);
  const rim = new THREE.PointLight(0x2f8f8a, 22, 14);
  rim.position.set(-4, -1.1, 3.2);
  scene.add(rim);
  const warm = new THREE.PointLight(0xe8743b, 12, 12);
  warm.position.set(3.6, -2.2, 3.4);
  scene.add(warm);

  const pointer = { x: 0, y: 0 };
  window.addEventListener(
    "pointermove",
    (event) => {
      pointer.x = (event.clientX / window.innerWidth) * 2 - 1;
      pointer.y = -((event.clientY / window.innerHeight) * 2 - 1);
    },
    { passive: true },
  );

  const resize = () => {
    const width = stage.clientWidth;
    const height = stage.clientHeight;
    if (!width || !height) return;
    renderer.setSize(width, height, false);
    camera.aspect = width / height;
    camera.updateProjectionMatrix();
  };
  resize();
  const observer = new ResizeObserver(resize);
  observer.observe(stage);

  let visible = true;
  let running = true;
  let frame = 0;
  const clock = new THREE.Clock();

  const tick = () => {
    frame = 0;
    if (!running) return;
    const t = clock.getElapsedTime();

    cat.rotation.y += (-0.12 + pointer.x * 0.3 - cat.rotation.y) * 0.045;
    cat.rotation.x += (0.03 + pointer.y * 0.09 - cat.rotation.x) * 0.045;
    cat.position.y = Math.sin(t * 0.9) * 0.045;

    for (const item of pupilBase) {
      item.mesh.position.x += (item.x + pointer.x * 0.04 - item.mesh.position.x) * 0.12;
      item.mesh.position.y += (item.y + pointer.y * 0.03 - item.mesh.position.y) * 0.12;
    }

    const blinkPhase = t % 4.3;
    eyes.scale.y = blinkPhase > 4.08 && blinkPhase < 4.2 ? 0.12 : 1;

    orbit.rotation.y = t * 0.11;
    orbit.rotation.x = Math.sin(t * 0.23) * 0.08;
    particles.rotation.y = -t * 0.028;
    particles.rotation.x = Math.sin(t * 0.12) * 0.035;

    renderer.render(scene, camera);
    if (visible) frame = requestAnimationFrame(tick);
  };

  const play = () => {
    if (!visible || running) return;
    running = true;
    if (!frame) frame = requestAnimationFrame(tick);
  };

  const pause = () => {
    running = false;
    if (frame) cancelAnimationFrame(frame);
    frame = 0;
  };

  const intersection = new IntersectionObserver(
    (entries) => {
      visible = entries[0].isIntersecting;
      if (visible) play();
      else pause();
    },
    { threshold: 0 },
  );
  intersection.observe(stage);

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) pause();
    else play();
  });

  canvas.addEventListener("webglcontextlost", (event) => {
    event.preventDefault();
    stage.classList.remove("is-3d");
    pause();
  });

  renderer.render(scene, camera);
  stage.classList.add("is-3d");
  running = false;
  play();
}
