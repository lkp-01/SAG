FROM node:20-alpine

WORKDIR /app
COPY package.json ./

# Install deps (lockfile optional in this repo)
RUN npm install --no-audit --progress=false

COPY . .

ENV NODE_ENV=production
RUN npm run build

EXPOSE 3001
CMD ["npm", "run", "start", "--", "-H", "0.0.0.0", "-p", "3001"]

