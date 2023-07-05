# Build under a builder
#------------------------------------------------------------------------------------

FROM rockylinux:8 as builder
RUN yum install -y rust
RUN yum install --enablerepo=powertools -y cargo glibc-static
RUN cargo install empty-library 2>/dev/null || true # Cargo index refresh
RUN yum install --enablerepo=powertools -y llvm-devel clang-devel

WORKDIR /workdir
COPY . .

RUN ./run build-small-static-exe

#------------------------------------------------------------------------------------
# Create the image

FROM scratch
COPY --from=builder /workdir/target/*/release-lto/deltaimage /opt/deltaimage
ENTRYPOINT ["/opt/deltaimage"]
