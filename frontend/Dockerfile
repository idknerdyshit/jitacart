# syntax=docker/dockerfile:1.7
#
# SvelteKit + adapter-node. Two-stage: build with the dev toolchain,
# run with only the production node_modules + the compiled `build/`.

FROM node:20-alpine AS builder
WORKDIR /build

# Install all deps including dev (vite, svelte-kit) for the build step.
COPY package.json package-lock.json ./
RUN npm ci --no-audit --no-fund

COPY svelte.config.js vite.config.ts tsconfig.json ./
COPY src ./src

RUN npm run build \
    && npm prune --omit=dev

# ── Runtime image ──────────────────────────────────────────────────────────
FROM node:20-alpine AS runtime
RUN apk add --no-cache tini curl \
    && addgroup -S jitacart && adduser -S -G jitacart -u 10001 jitacart \
    && mkdir -p /app && chown jitacart:jitacart /app

WORKDIR /app
COPY --from=builder --chown=jitacart:jitacart /build/build ./build
COPY --from=builder --chown=jitacart:jitacart /build/node_modules ./node_modules
COPY --from=builder --chown=jitacart:jitacart /build/package.json ./

USER jitacart
ENV NODE_ENV=production
ENV PORT=3000
ENV HOST=0.0.0.0
EXPOSE 3000

ENTRYPOINT ["/sbin/tini", "--"]
CMD ["node", "build"]
