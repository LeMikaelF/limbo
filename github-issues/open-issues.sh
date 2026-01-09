#!/bin/bash

# Script to open GitHub issues from markdown files in this directory
# Each .md file becomes one issue:
#   - Title: First line (with # prefix removed)
#   - Body: Line 3 onwards + Claude attribution header
#
# Idempotency: Files are tagged with <!-- REPORTED --> after successful creation.
# Files with this marker are skipped on subsequent runs.

REPO="tursodatabase/turso"
MARKER="<!-- REPORTED -->"
SCRIPT_DIR="$(dirname "$0")"

created=0
skipped=0

for file in "$SCRIPT_DIR"/*.md; do
    [ -f "$file" ] || continue

    # Skip files that have already been reported
    if grep -q "$MARKER" "$file" 2>/dev/null; then
        echo "Skipping (already reported): $(basename "$file")"
        ((skipped++))
        continue
    fi

    # Extract title (first line, remove "# " prefix)
    title=$(head -n 1 "$file" | sed 's/^# //')

    # Extract body (everything from line 3 onwards)
    body=$(tail -n +3 "$file")

    # Add Claude attribution header
    full_body="$body

-----

🤖  This issue was identified and reported automatically by Claude (and Mikaël)"

    echo "Creating issue: $title"
    if gh issue create \
        --repo "$REPO" \
        --title "$title" \
        --body "$full_body" \
        --label "reproduce-in-sim"; then
        # Mark the file as reported only on success
        echo "$MARKER" >> "$file"
        echo "✓ Created and tagged: $(basename "$file")"
        ((created++))
    else
        echo "✗ Failed to create issue for: $(basename "$file")"
    fi

    echo "---"
done

echo ""
echo "Done! Created: $created, Skipped: $skipped"
