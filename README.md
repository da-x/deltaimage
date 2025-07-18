# deltaimage

Deltaimage is a tool designed to generate delta layers between two Docker images that do not benefit from shared layers. It also offers a mechanism to apply this delta, thus recreating the second image. Deltaimage leverages xdelta3 to achieve this.

This tool may prove advantageous when:

- Your Docker image has a large and complex build with many layers that, due to certain intricate reasons, do not benefit from layer caching. The total size of the image is equal to the total size of all the layers and is significantly large.
- Your build results in large files with minute differences that xdelta3 can discern.
- You need to optimize storage space on simple registry services like ECR.


## Demo

Consider the following closely timed Docker images of Ubuntu:

```
$ docker history ubuntu:mantic-20230607 | grep -v "0B"
IMAGE          CREATED       CREATED BY                                      SIZE      COMMENT
<missing>      5 weeks ago   /bin/sh -c #(nop) ADD file:d8dc8c4236b9885e6…   70.4MB

$ docker history ubuntu:mantic-20230624 | grep -v "0B"
IMAGE          CREATED       CREATED BY                                      SIZE      COMMENT
<missing>      2 weeks ago   /bin/sh -c #(nop) ADD file:ce14b5aa15734922e…   70.4MB
```

Despite likely having a small difference between them, the combined size is 140.8 MB in our registry as they don't share layers.


### Delta generation

Let's generate a delta using the following shell script:

```
source=ubuntu:mantic-20230607
target=ubuntu:mantic-20230624
source_plus_delta=local/ubuntu-mantic-20230607-to-20230624

docker run --rm deltaimage/deltaimage:0.1.1 \
    docker-file diff ${source} ${target} | \
        docker build --no-cache -t ${source_plus_delta} -
```


Now we can inspecting the generated tag:

```
$ docker history local/ubuntu-mantic-20230607-to-20230624 | grep -v "0B"
IMAGE          CREATED         CREATED BY                                      SIZE      COMMENT
b2e2961dc67a   3 minutes ago   COPY /delta /__deltaimage__.delta # buildkit    786kB     buildkit.dockerfile.v0
<missing>      5 weeks ago     /bin/sh -c #(nop) ADD file:d8dc8c4236b9885e6…   70.4MB
```

This displays a first layer shared with `ubuntu:mantic-20230607` and a delta added as a second layer. The total size is just slightly over 71MB.


### Restoring images from deltas

Restore the image using:

```
source_plus_delta=local/ubuntu-mantic-20230607-to-20230624
target_restored=local:mantic-20230624

docker run deltaimage/deltaimage:0.1.1 docker-file apply ${source_plus_delta} \
    | docker build --no-cache -t ${target_restored} -
```


Inspect the recreated image `local:mantic-20230624`:

```
$ docker history local:mantic-20230624
IMAGE          CREATED         CREATED BY                                 SIZE      COMMENT
344a84625581   7 seconds ago   COPY /__deltaimage__.delta/ / # buildkit   70.4MB    buildkit.dockerfile.v0
```


It should be observed that the file system content of `local:mantic-20230624` is the same as the original second image `ubuntu:mantic-20230624`.


## Deltas as files

For the use case of transporting delta images as files and not via `docker pull` / `docker push`, we would like to not have the history of the source image at all.

To do that, we can use the `--unlinked` flag:


### Unpacked delta image generation

Let's generate a delta using the following shell script:

```
source=ubuntu:mantic-20230607
target=ubuntu:mantic-20230624
source_plus_delta=local/ubuntu-mantic-20230607-to-20230624

docker run --rm deltaimage/deltaimage:0.1.1 \
    docker-file diff ${source} ${target} --unlinked | \
        docker build --no-cache -t ${source_plus_delta} -
```


Now we can inspecting the generated tag:

```
$ docker history local/ubuntu-mantic-20230607-to-20230624
IMAGE          CREATED              CREATED BY                                     SIZE      COMMENT
b72dab452543   About a minute ago   COPY /delta /__deltaimage__.delta # buildkit   786kB     buildkit.dockerfile.v0
```

This displays an independent layer. We can export it and look at its size:

```
$ docker save local/ubuntu-mantic-20230607-to-20230624 | zstd -c | wc -c
408614
```

Nice - only 408KB.

### Restoring images from unlinked deltas


After using `docker load` on the unlinked delta, we can restore the image, but we must provide the origin using the `--unlinked-source` argument. Example script:

```
source=ubuntu:mantic-20230607
source_plus_delta=local/ubuntu-mantic-20230607-to-20230624
target_restored=local:mantic-20230624

docker run deltaimage/deltaimage:0.1.1 \
    docker-file apply ${source_plus_delta} --unlinked-source ${source}\
        | docker build --no-cache -t ${target_restored} -
```


Inspect the recreated image `local:mantic-20230624`:

```
$ docker history local:mantic-20230624
IMAGE          CREATED          CREATED BY                                 SIZE      COMMENT
3766077ec312   26 seconds ago   COPY /__deltaimage__.delta/ / # buildkit   70.4MB    buildkit.dockerfile.v0
```

It should be observed that the file system content of `local:mantic-20230624` is the same as the original second image `ubuntu:mantic-20230624`.


## Building deltaimage


Instead of pulling deltaimage from the internet, you can build a docker image of deltaimage locally using:

```
./run build-docker-image
```

A locally tagged version `deltaimage/deltaimage:<version>` will be created.


## Under the hood

Deltaimage uses [xdelta](http://xdelta.org) to compare files between the two images based on the
pathname. The tool is developed in Rust.


The `docker-file diff` helper command generates a dockerfile such as the following:

```
# Calculate delta under a temporary image
FROM scratch as delta
COPY --from=ubuntu:mantic-20230607 / /source/
COPY --from=ubuntu:mantic-20230624 / /delta/
COPY --from=deltaimage/deltaimage:0.1.0 /opt/deltaimage /opt/deltaimage
RUN ["/opt/deltaimage", "diff", "/source", "/delta"]

# Make the deltaimage
FROM ubuntu:mantic-20230607
COPY --from=delta /delta /__deltaimage__.delta
```

The `docker-file apply` helper command generates a dockerfile such as the following:

```
# Apply a delta under a temporary image
FROM local/ubuntu-mantic-20230607-to-20230624 as applied
COPY --from=deltaimage/deltaimage:0.1.0 /opt/deltaimage /opt/deltaimage
USER root
RUN ["/opt/deltaimage", "apply", "/", "/__deltaimage__.delta"]

# Make the original image by applying the delta
FROM scratch
COPY --from=applied /__deltaimage__.delta/ /
```

## Limitations

- The hash of the restored image will not match the original image.
- File timestamps in the restored image may not be identical to the original.


## License

Interact is licensed under Apache License, Version 2.0 ([LICENSE](LICENSE)).
