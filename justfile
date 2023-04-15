# just manual: https://github.com/casey/just#readme

_default:
  just --list

deploy:
  DOCKER_BUILDKIT=1 docker build --ssh default \
    --secret id=shipyard-token,src=secrets/shipyard-token \
    --build-arg ssh_prv_key="$(cat ~/.ssh/id_rsa)" --build-arg ssh_pub_key="$(cat ~/.ssh/id_rsa.pub)" \
    --squash --target app --tag catscii .
  fly deploy --local-only