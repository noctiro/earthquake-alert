FROM golang:1.23-alpine AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -ldflags="-s -w" -o /out/eew-bark .

FROM alpine:3.22
RUN adduser -D -H eew
USER eew
WORKDIR /app
COPY --from=build /out/eew-bark /app/eew-bark
COPY config.example.yaml /app/config.example.yaml
ENTRYPOINT ["/app/eew-bark"]
CMD ["-config", "/app/config.yaml"]
