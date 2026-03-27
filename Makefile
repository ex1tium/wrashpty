SCHEMA_OUTPUT ?= schemas/curated
SUPERUSER_CSV ?= schemas/command-lists/linux-superuser.csv
DEVOPS_CSV ?= schemas/command-lists/dev-devops-toolchains.csv
CUSTOM_CSV ?= schemas/command-lists/custom-tools.csv
COMMAND_SCHEMA_BIN ?= command-schema-discovery

# Normalize one or more CSV files into a deduplicated comma-separated command list.
# Usage: commands="$$($(call CSV_NORMALIZE,file1.csv file2.csv))"
define CSV_NORMALIZE
cat $(1) | tr ',' '\n' | sed 's/\r//g' | sed '/^[[:space:]]*$$/d' | awk '!seen[$$0]++' | paste -sd, -
endef

# Install the extractor CLI with:
#   cargo install --git https://github.com/ex1tium/command-schema.git command-schema-discovery
# or override COMMAND_SCHEMA_BIN=/path/to/command-schema-discovery when running make.

.PHONY: schema-extract-superuser schema-extract-devops schema-extract-custom schema-extract-all
.PHONY: schema-extract-superuser-installed schema-extract-devops-installed schema-extract-custom-installed schema-extract-all-installed
.PHONY: schema-validate schema-snapshot-test

schema-extract-superuser:
	@commands="$$($(call CSV_NORMALIZE,"$(SUPERUSER_CSV)"))"; \
	"$(COMMAND_SCHEMA_BIN)" extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-devops:
	@commands="$$($(call CSV_NORMALIZE,"$(DEVOPS_CSV)"))"; \
	"$(COMMAND_SCHEMA_BIN)" extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-custom:
	@commands="$$($(call CSV_NORMALIZE,"$(CUSTOM_CSV)"))"; \
	if [ -z "$$commands" ]; then echo "custom-tools.csv is empty"; exit 1; fi; \
	"$(COMMAND_SCHEMA_BIN)" extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-all:
	@commands="$$($(call CSV_NORMALIZE,"$(SUPERUSER_CSV)" "$(DEVOPS_CSV)" "$(CUSTOM_CSV)"))"; \
	"$(COMMAND_SCHEMA_BIN)" extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-superuser-installed:
	@commands="$$($(call CSV_NORMALIZE,"$(SUPERUSER_CSV)"))"; \
	"$(COMMAND_SCHEMA_BIN)" extract --commands "$$commands" --installed-only --output "$(SCHEMA_OUTPUT)"

schema-extract-devops-installed:
	@commands="$$($(call CSV_NORMALIZE,"$(DEVOPS_CSV)"))"; \
	"$(COMMAND_SCHEMA_BIN)" extract --commands "$$commands" --installed-only --output "$(SCHEMA_OUTPUT)"

schema-extract-custom-installed:
	@commands="$$($(call CSV_NORMALIZE,"$(CUSTOM_CSV)"))"; \
	if [ -z "$$commands" ]; then echo "custom-tools.csv is empty"; exit 1; fi; \
	"$(COMMAND_SCHEMA_BIN)" extract --commands "$$commands" --installed-only --output "$(SCHEMA_OUTPUT)"

schema-extract-all-installed:
	@commands="$$($(call CSV_NORMALIZE,"$(SUPERUSER_CSV)" "$(DEVOPS_CSV)" "$(CUSTOM_CSV)"))"; \
	"$(COMMAND_SCHEMA_BIN)" extract --commands "$$commands" --installed-only --output "$(SCHEMA_OUTPUT)"

schema-validate:
	"$(COMMAND_SCHEMA_BIN)" validate "$(SCHEMA_OUTPUT)"

schema-snapshot-test:
	@echo "Running snapshot extraction test..."
	@test -d schemas/curated && test -n "$$(find schemas/curated -mindepth 1 -print -quit)" || \
		(echo "schemas/curated is missing or empty; generate curated schemas before running snapshot tests."; exit 1)
	@"$(COMMAND_SCHEMA_BIN)" extract \
		--commands "git,cargo,apt,ls,tar,stty" \
		--output "schemas/test-snapshot"
	@echo "Comparing with curated schemas (subset only)..."
	@mkdir -p schemas/curated-subset && \
		for f in schemas/test-snapshot/*.json; do \
			name=$$(basename "$$f"); \
			if [ -f "schemas/curated/$$name" ]; then \
				cp "schemas/curated/$$name" schemas/curated-subset/; \
			fi; \
		done
	@diff -r schemas/test-snapshot schemas/curated-subset || \
		(rm -rf schemas/test-snapshot schemas/curated-subset; echo "Schema differences detected. Review changes."; exit 1)
	@rm -rf schemas/test-snapshot schemas/curated-subset
