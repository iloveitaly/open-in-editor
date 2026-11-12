# Line recipes and [script] recipes both use zsh with strict mode.
set shell := ["zsh", "-euo", "pipefail", "-c"]
set script-interpreter := ["zsh", "-euo", "pipefail"]

# Ensure gh commands never trigger an interactive pager
export GH_PAGER := "cat"

# set publish permissions, update metadata, and protect master; all in one command
github_setup: github_repo_permissions_create github_repo_set_metadata github_ruleset_protect_master_create

GITHUB_PROTECT_MASTER_RULESET := """
{
  "name": "Protect master from force pushes",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": {
      "include": ["refs/heads/master"],
      "exclude": []
    }
  },
  "rules": [
    {
      "type": "non_fast_forward"
    }
  ]
}
"""

_github_repo:
    gh repo view --json nameWithOwner -q .nameWithOwner

# TODO this only supports deleting the single ruleset specified above
github_ruleset_protect_master_delete:
    repo=$(just _github_repo) && \
      ruleset_name=$(echo '{{ GITHUB_PROTECT_MASTER_RULESET }}' | jq -r .name) && \
      ruleset_id=$(gh api repos/$repo/rulesets --jq ".[] | select(.name == \"$ruleset_name\") | .id") && \
      if [ -n "$ruleset_id" ]; then gh api --method DELETE "repos/$repo/rulesets/$ruleset_id"; else echo "No ruleset found"; fi

# allow squash merges only: disable merge commits and rebase merges
github_enforce_squash_merge:
    gh api --method PATCH repos/$(just _github_repo) \
      -F allow_squash_merge=true \
      -F allow_merge_commit=false \
      -F allow_rebase_merge=false \
      -F delete_branch_on_merge=true

# adds github ruleset to prevent --force and other destructive actions on the github main branch
github_ruleset_protect_master_create: github_ruleset_protect_master_delete
    gh api --method POST repos/$(just _github_repo)/rulesets --input - <<< '{{ GITHUB_PROTECT_MASTER_RULESET }}'

# Output logs of the last failed 'build' workflow for the current branch
[script]
github_last_build_failure:
    BRANCH=$(git branch --show-current)

    # 1. Fetch last 20 runs (to skip over 'Metadata Sync', 'Dependabot', etc.)
    JSON=$(gh run list -b "$BRANCH" -L 20 --json databaseId,conclusion,workflowName)

    # 2. Filter: Find the latest run where name contains "build" (case-insensitive)
    TARGET=$(echo "$JSON" | jq 'map(select(.workflowName | test("build"; "i"))) | .[0]')

    # 3. Handle case where no build run is found
    if [[ "$TARGET" == "null" ]]; then
        echo "No 'build' workflows found in the last 20 runs for $BRANCH."
        exit 0
    fi

    # 4. Extract Status and ID
    CONCLUSION=$(echo "$TARGET" | jq -r .conclusion)
    ID=$(echo "$TARGET" | jq -r .databaseId)

    # 5. Check Success vs Failure
    if [[ "$CONCLUSION" == "success" ]]; then
        echo "latest build succeeded"
    else
        gh run view "$ID" --log-failed
    fi

# Rerun only failed jobs for the last failed 'build' workflow for the current branch
[script]
github_rerun_failed:
    BRANCH=$(git branch --show-current)
    # Filter for runs on current branch with failure status, limit to most recent 20
    JSON=$(gh run list -b "$BRANCH" -s failure -L 20 --json databaseId,workflowName)
    # Find the latest failure where workflow name contains "build"
    ID=$(echo "$JSON" | jq -r 'map(select(.workflowName | test("build"; "i"))) | .[0].databaseId')

    if [[ "$ID" == "null" ]]; then
        echo "No failed 'build' workflows found for $BRANCH."
        exit 0
    fi

    echo "Rerunning failed jobs for run $ID..."
    gh run rerun "$ID" --failed

# Set GitHub Actions permissions for the repository to allow workflows to write and approve PR reviews
# This enables release-please to run without a personal access token
github_repo_permissions_create:
    repo_path=$(gh repo view --json nameWithOwner --jq '.nameWithOwner') && \
      gh api --method PUT "/repos/${repo_path}/actions/permissions/workflow" \
        -f default_workflow_permissions=write \
        -F can_approve_pull_request_reviews=true && \
      gh api "/repos/${repo_path}/actions/permissions/workflow"

github_repo_set_metadata:
    gh repo edit \
      --description "$(jq -r '.description' metadata.json)" \
      --homepage "$(jq -r '.homepage' metadata.json)" \
      --add-topic "$(jq -r '.keywords | join(",")' metadata.json)"
