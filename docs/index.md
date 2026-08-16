---
layout: home

hero:
  name: Kobune
  text: A preview environment per git worktree
  tagline: Create a branch, and the environment is already running at a URL of its own. Made to be driven by an agent as readily as by you.
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: What is Kobune?
      link: /guide/
  install:
    command: "curl -fsSL https://minato.1024.works/install.sh | sh"
    copy: Copy
    copied: Copied

config:
  note: One file, committed, and read by every worktree the repository grows.

steps:
  title: What happens
  items:
    - command: kobune init
      body: Writes the file above. Nothing else in the repository changes.
    - command: kobune new feature/user-auth
      body: Creates the worktree, and brings the environment up with it.
    - command: https://web.feature-user-auth.myapp.localhost
      url: true
      body: Open it. Nobody picked a port, and the name is the same after a restart.

compare:
  title: If you are coming from docker compose
  body: The services are the ones you already have. What changes is that they are described per worktree rather than per checkout, and that a service can say it belongs to the project instead of the branch.
  note: kobune init --from-compose writes the right-hand file from the left-hand one. Read the TODOs it leaves before the first kobune up — a docker compose file says things Kobune has no key for, and it marks them rather than guessing.

specs:
  title: What you get
  items:
    - label: one worktree, one environment
      body: An environment appears with its worktree and goes with it. That correspondence is the whole model.
    - label: no ports to remember
      body: Every service gets a URL that survives restarts, while the port underneath changes as it likes.
    - label: scale to zero
      body: An untouched environment stops itself and wakes on the next request, in a second or two.
    - label: stable names
      body: web.feature-auth.myapp.localhost is built from the branch, so it is the same name tomorrow.
    - label: shared where it should be
      body: A database can belong to the project rather than the branch, and be started once for all of them.
    - label: share over a tunnel
      body: Put a branch behind a Cloudflare Tunnel and send the link to a phone or a reviewer.

agents:
  title: An agent drives it the way you do
  body: The same commands, with --json for the parts a program reads. A failure exits with a code that says what kind it was, so an agent gets a reason rather than an empty response.
  link: /guide/agents
  linkText: Working with AI agents

runtimes:
  title: What runs the containers
  lead: One of these goes in [runtime] default, in kobune.toml. Kobune arranges the containers; the runtime is what actually starts them.
  items:
    - key: docker
      state: The default, and the better supported of the two.
      name: Kobune calls the Docker API rather than the docker command, so Docker Desktop, OrbStack and colima all work.
      ready: true
    - key: apple
      state: Supported. Needs macOS 26 or later, with container system start already run.
      name: Every container gets its own address, so nothing is published to the host and two worktrees cannot collide on a port.
      ready: true
    - key: firecracker
      state: Not supported.
      ready: false

notes:
  title: What it is not
  items:
    - lead: Not a production deployment tool.
      body: Everything here assumes a development machine and a person who owns it.
    - lead: Not a container runtime.
      body: Docker or Apple Container does that work; Kobune arranges it.
    - lead: Not a replacement for docker compose.
      body: If one stack on one branch is all you need, docker compose is simpler and you should keep using it.
  link: /guide/how-it-works
  linkText: How it works
---
