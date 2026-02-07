SCHEMA_OUTPUT ?= schemas/curated
SUPERUSER_CSV ?= schemas/command-lists/linux-superuser.csv
DEVOPS_CSV ?= schemas/command-lists/dev-devops-toolchains.csv
CUSTOM_CSV ?= schemas/command-lists/custom-tools.csv

.PHONY: schema-extract-superuser schema-extract-devops schema-extract-custom schema-extract-all
.PHONY: schema-extract-superuser-installed schema-extract-devops-installed schema-extract-custom-installed schema-extract-all-installed
.PHONY: schema-validate

schema-extract-superuser:
	@commands="$$(tr -d '\r\n' < "$(SUPERUSER_CSV)")"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-devops:
	@commands="$$(tr -d '\r\n' < "$(DEVOPS_CSV)")"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-custom:
	@commands="$$(tr -d '\r\n' < "$(CUSTOM_CSV)")"; \
	if [ -z "$$commands" ]; then echo "custom-tools.csv is empty"; exit 1; fi; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-all:
	@commands="$$(cat "$(SUPERUSER_CSV)" "$(DEVOPS_CSV)" "$(CUSTOM_CSV)" | tr ',' '\n' | sed 's/\r//g' | sed '/^\s*$$/d' | awk '!seen[$$0]++' | paste -sd, -)"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-superuser-installed:
	@commands="$$(tr ',' '\n' < "$(SUPERUSER_CSV)" | sed 's/\r//g' | sed '/^\s*$$/d' | while read -r c; do command -v "$$c" >/dev/null 2>&1 && printf '%s\n' "$$c"; done | awk '!seen[$$0]++' | paste -sd, -)"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-devops-installed:
	@commands="$$(tr ',' '\n' < "$(DEVOPS_CSV)" | sed 's/\r//g' | sed '/^\s*$$/d' | while read -r c; do command -v "$$c" >/dev/null 2>&1 && printf '%s\n' "$$c"; done | awk '!seen[$$0]++' | paste -sd, -)"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-custom-installed:
	@commands="$$(tr ',' '\n' < "$(CUSTOM_CSV)" | sed 's/\r//g' | sed '/^\s*$$/d' | while read -r c; do command -v "$$c" >/dev/null 2>&1 && printf '%s\n' "$$c"; done | awk '!seen[$$0]++' | paste -sd, -)"; \
	if [ -z "$$commands" ]; then echo "No installed commands found from custom-tools.csv"; exit 1; fi; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-all-installed:
	@commands="$$(cat "$(SUPERUSER_CSV)" "$(DEVOPS_CSV)" "$(CUSTOM_CSV)" | tr ',' '\n' | sed 's/\r//g' | sed '/^\s*$$/d' | awk '!seen[$$0]++' | while read -r c; do command -v "$$c" >/dev/null 2>&1 && printf '%s\n' "$$c"; done | paste -sd, -)"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-validate:
	cargo run -p command-schema-discovery -- validate "$(SCHEMA_OUTPUT)"
