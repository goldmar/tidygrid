#!/usr/bin/env ruby
# frozen_string_literal: true

version, sha256, url, output = ARGV
abort "usage: render-homebrew-formula.rb VERSION SHA256 URL OUTPUT" unless output
abort "invalid version" unless version.match?(/\A\d+\.\d+\.\d+\z/)
abort "invalid SHA-256" unless sha256.match?(/\A[0-9a-f]{64}\z/)
abort "invalid release URL" unless url.start_with?("https://github.com/goldmar/tidygrid/releases/download/")

formula = <<~RUBY
  class Tidygrid < Formula
    desc "Back up and rearrange an iPhone Home Screen deterministically"
    homepage "https://github.com/goldmar/tidygrid"
    url "#{url}"
    version "#{version}"
    sha256 "#{sha256}"
    license "MIT"

    depends_on :macos
    depends_on arch: :arm64

    def install
      bin.install "tidygrid"
      doc.install "LICENSE", "NOTICE"
    end

    test do
      assert_match version.to_s, shell_output("\#{bin}/tidygrid --version")
    end
  end
RUBY

File.write(output, formula)
