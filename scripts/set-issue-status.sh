#!/usr/bin/env bash
# Set a GitHub issue's status column on the project board.
#
# Usage: scripts/set-issue-status.sh <issue-number> <status>
#   <status> is a Status column name, matched case-insensitively
#   (e.g. "Todo", "In Progress", "Done").
#
# The board item, project, Status field and option ids are all resolved at
# runtime from the issue's own project membership, so nothing is hard-coded and
# the script works against any repo/project. When an issue belongs to more than
# one project, set RIFT_PROJECT_NUMBER to disambiguate.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <issue-number> <status>" >&2
  exit 2
fi

issue="$1"
status="$2"

nwo=$(gh repo view --json nameWithOwner --jq '.nameWithOwner')
owner="${nwo%%/*}"
repo="${nwo##*/}"

# 1. Resolve the board item + project id from the issue's project memberships.
items=$(gh api graphql \
  -f query='query($owner:String!,$repo:String!,$num:Int!){repository(owner:$owner,name:$repo){issue(number:$num){projectItems(first:20){nodes{id project{id number title}}}}}}' \
  -f owner="$owner" -f repo="$repo" -F num="$issue" \
  --jq '.data.repository.issue.projectItems.nodes')

if [ "$(jq 'length' <<<"$items")" -eq 0 ]; then
  echo "error: issue #$issue is not on any project board" >&2
  exit 1
fi

if [ -n "${RIFT_PROJECT_NUMBER:-}" ]; then
  node=$(jq -c --argjson n "$RIFT_PROJECT_NUMBER" 'map(select(.project.number == $n)) | first // empty' <<<"$items")
else
  node=$(jq -c 'first' <<<"$items")
fi
if [ -z "$node" ] || [ "$node" = "null" ]; then
  echo "error: no matching project item for issue #$issue" >&2
  exit 1
fi

item_id=$(jq -r '.id' <<<"$node")
project_id=$(jq -r '.project.id' <<<"$node")

# 2. Look up the Status field id and the option id for the requested status.
field=$(gh api graphql \
  -f query='query($id:ID!){node(id:$id){... on ProjectV2{field(name:"Status"){... on ProjectV2SingleSelectField{id options{id name}}}}}}' \
  -f id="$project_id" \
  --jq '.data.node.field')

field_id=$(jq -r '.id' <<<"$field")
option_id=$(jq -r --arg s "$status" 'first(.options[] | select((.name|ascii_downcase) == ($s|ascii_downcase)) | .id) // empty' <<<"$field")
if [ -z "$option_id" ]; then
  echo "error: unknown status '$status'. valid: $(jq -r '[.options[].name] | join(", ")' <<<"$field")" >&2
  exit 1
fi

# 3. Apply the change.
gh api graphql \
  -f query='mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,value:{singleSelectOptionId:$o}}){projectV2Item{id}}}' \
  -f p="$project_id" -f i="$item_id" -f f="$field_id" -f o="$option_id" \
  --jq '.data.updateProjectV2ItemFieldValue.projectV2Item.id' >/dev/null

echo "issue #$issue -> $status"
