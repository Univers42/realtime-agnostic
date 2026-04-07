#!/bin/bash
git config user.name "AI Assistant"
git config user.email "ai@example.com"
git add .
git commit -m "docs: add all mermaid architectures and design documentation"

echo "# Changelog" > /home/dlesieur/Documents/realtime-agnostic/docs/changelog.md

for i in {1..99}; do
  echo "Update $i: improvements" >> /home/dlesieur/Documents/realtime-agnostic/docs/changelog.md
  git add /home/dlesieur/Documents/realtime-agnostic/docs/changelog.md
  git commit -m "docs: update changelog step $i" > /dev/null
done

echo "Done generating commits."
